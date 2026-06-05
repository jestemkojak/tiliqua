#![no_std]
#![no_main]

use core::fmt::Write as FmtWrite;

use log::info;
use riscv_rt::entry;

use tiliqua_pac as pac;
use tiliqua_fw::{bootstrap, fat, psid, usb_msc::UsbMsc};
use tiliqua_fw::*;

use tiliqua_lib::*;
use tiliqua_lib::color::HI8;
use pac::constants::*;

use tiliqua_hal::persist::Persist;
use tiliqua_lib::scope::{Timebase, VScale};
use tiliqua_hal::embedded_graphics::primitives::{Rectangle, PrimitiveStyle};
use tiliqua_hal::embedded_graphics::geometry::Size;

use tiliqua_hal::encoder::Encoder;
use tiliqua_hal::embedded_graphics::{
    prelude::*,
    mono_font::{MonoTextStyle, ascii::FONT_9X15_BOLD},
    text::Text,
    geometry::Point,
};

use heapless::String;

/// PSRAM base address of the 6502's 64KB view.
const CPU6502_PSRAM_BASE: usize = 0x20800000;

/// Write bytes into the 6502's view of PSRAM (volatile, byte-by-byte).
fn write_6502_mem(base: usize, addr: u16, data: &[u8]) {
    let ptr = (base + addr as usize) as *mut u8;
    for (i, &b) in data.iter().enumerate() {
        unsafe { core::ptr::write_volatile(ptr.add(i), b); }
    }
}

/// Force the RISC-V write-back L1 D-cache out to physical PSRAM.
///
/// The 6502 reads PSRAM through a *separate* wishbone master that does not see
/// the RISC-V's private L1 cache. Stores from `write_6502_mem` linger dirty in
/// L1 (`write_volatile` does NOT bypass the hardware cache), so the 6502 would
/// otherwise fetch stale/zero bytes — including a null reset vector — and never
/// run the tune. VexiiRiscv here has no usable cache-maintenance instruction,
/// so we evict by thrashing: read a scratch region many times larger than the
/// L1 and disjoint from the 6502 window (0x20800000+). Every cache set is
/// refilled with clean scratch lines, writing back all dirty image lines.
/// Must be called after writing the image and before releasing the 6502.
fn flush_6502_image() {
    // PSRAM base, well below the 6502 window — reads here are side-effect-free.
    const SCRATCH: *const u32 = 0x2000_0000 as *const u32;
    const WORDS: usize = (64 * 1024) / 4; // 64 KiB >> any L1 config on this SoC
    let mut acc: u32 = 0;
    for i in 0..WORDS {
        acc = acc.wrapping_add(unsafe { core::ptr::read_volatile(SCRATCH.add(i)) });
    }
    core::hint::black_box(acc);
}

/// Extract a null-terminated ASCII string from a fixed-width byte slice.
fn trim_ascii(s: &[u8]) -> &str {
    let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
    core::str::from_utf8(&s[..end]).unwrap_or("?")
}

fn load_and_start(
    tune_buf: &[u8],
    len: usize,
    hdr: &mut psid::PsidHeader,
    subtune: u16,
    play_timer: &pac::PLAY_TIMER,
    paused: bool,
) {
    *hdr = psid::PsidHeader::parse(&tune_buf[..len]).expect("bad PSID");

    let payload_raw = &tune_buf[hdr.data_offset as usize..len];
    let load_addr   = hdr.effective_load_addr(payload_raw);
    let payload     = if hdr.load_addr == 0 { &payload_raw[2..] } else { payload_raw };
    write_6502_mem(CPU6502_PSRAM_BASE, load_addr, payload);
    write_6502_mem(CPU6502_PSRAM_BASE, bootstrap::INIT_STUB_ADDR,
                   &bootstrap::init_stub((subtune.saturating_sub(1)) as u8, hdr.init_addr));
    write_6502_mem(CPU6502_PSRAM_BASE, bootstrap::NMI_STUB_ADDR,
                   &bootstrap::nmi_stub(hdr.play_addr));
    write_6502_mem(CPU6502_PSRAM_BASE, 0xFFFA, &bootstrap::vectors());
    flush_6502_image();

    let ntsc = hdr.is_ntsc(subtune);
    play_timer.control().write(|w| {
        w.reset().set_bit().play_rate().bit(ntsc).irq_enable().clear_bit()
    });
    play_timer.control().write(|w| {
        w.reset().clear_bit().play_rate().bit(ntsc).irq_enable().clear_bit()
    });
    for _ in 0..2_000_000u32 { unsafe { core::arch::asm!("nop"); } }
    if !paused {
        play_timer.control().write(|w| {
            w.reset().clear_bit().play_rate().bit(ntsc).irq_enable().set_bit()
        });
    }
}

#[entry]
fn main() -> ! {
    let peripherals = pac::Peripherals::take().unwrap();
    let serial = Serial0::new(peripherals.UART0);

    tiliqua_fw::handlers::logger_init(serial);
    info!("Hello from SID Player!");

    let bootinfo = unsafe { bootinfo::BootInfo::from_addr(BOOTINFO_BASE) }.unwrap();
    let modeline = bootinfo.modeline.maybe_override_fixed(FIXED_MODELINE, CLOCK_DVI_HZ);
    let mut display = DMAFramebuffer0::new(
        peripherals.FRAMEBUFFER_PERIPH,
        peripherals.PALETTE_PERIPH,
        peripherals.BLIT,
        peripherals.PIXEL_PLOT,
        peripherals.LINE,
        PSRAM_FB_BASE,
        modeline,
        BLIT_MEM_BASE,
    );

    palette::ColorPalette::default().write_to_hardware(&mut display);

    // Framebuffer geometry comes from the bootloader-detected modeline, not a
    // fixed 640x480 — read it and lay everything out relative to it.
    let h_active = display.size().width  as i16;
    let v_active = display.size().height as i16;
    const HEADER_H: i16 = 56; // pixels reserved at top for the text header

    // --- Voice scope: fixed config, always on -----------------------------
    let mut scope   = Scope0::new(peripherals.SCOPE_PERIPH, 6);
    let mut persist = Persist0::new(peripherals.PERSIST_PERIPH);

    // Persistence = decay rate. Too low (fast decay) makes the free-running
    // traces strobe/flicker; this value lets successive sweeps overlay into a
    // stable band.
    persist.set_persistence(10);

    scope.set_intensity(8);
    // Scale the traces down ~50% in both axes (default scale shift is 6 → 7).
    // Smaller in X also packs the samples denser per pixel, reducing the
    // dotted look. (The fully-continuous fix is gateware upsampling as in
    // macro_osc; these are the firmware-only levers.)
    scope.set_yscale(VScale::Scale2V); // 2V/div = half the amplitude of 1V/div
    scope.set_xscale(7);               // half the horizontal extent
    // Slower timebase also packs more samples per pixel (less dotted).
    scope.set_timebase(Timebase::Timebase10ms);
    scope.set_trigger_level(0);
    scope.set_hue(0);          // per-channel hue is auto-offset (+3 per ch)
    scope.set_xpos_px(0);

    // Stack the four traces (V1/V2/V3/MIX) evenly in the band below the header.
    // ypos is a signed offset from screen centre (OffsetMode.CENTER in gateware).
    let centre = v_active / 2;
    for ch in 0..4i16 {
        let row = HEADER_H + ((ch * 2 + 1) * (v_active - HEADER_H)) / 8;
        scope.set_ypos_px(ch as usize, row - centre);
    }

    // Free-run (trigger_always = true) so traces show without a trigger edge.
    scope.set_enabled(true, true);

    // Fallback tune embedded at build time — plays if no USB drive is present.
    static FALLBACK_SID: &[u8] = include_bytes!("../Gyroscope_3.sid");

    // -----------------------------------------------------------------
    // Step 1: Show banner, try USB for ~2 s, fall back to built-in tune.
    // -----------------------------------------------------------------
    let style     = MonoTextStyle::new(&FONT_9X15_BOLD, HI8::new(0, 0xB));
    let style_dim = MonoTextStyle::new(&FONT_9X15_BOLD, HI8::new(0, 0x7));

    Text::new("SID PLAYER", Point::new(20, 20), style)
        .draw(&mut display)
        .ok();
    Text::new("Insert USB drive, or plays built-in tune...", Point::new(20, 50), style_dim)
        .draw(&mut display)
        .ok();

    let msc = UsbMsc::new(peripherals.USB_MSC);

    // Poll for USB readiness for ~2 s (60 MHz * 2 = 120_000_000 nops).
    const USB_TIMEOUT: u32 = 120_000_000;
    let usb_ready = {
        let mut ready = false;
        for _ in 0..USB_TIMEOUT {
            if msc.ready() { ready = true; break; }
            unsafe { core::arch::asm!("nop"); }
        }
        ready
    };

    // Scratch buffer in PSRAM at +7 MB (well away from framebuffer at 0x20000000).
    let tune_buf: &mut [u8] = unsafe {
        core::slice::from_raw_parts_mut((PSRAM_BASE + 0x700000) as *mut u8, 65536)
    };

    // Enumerate root *.SID filenames once (load payloads only on commit).
    let mut file_list: sid_scan::SidList = sid_scan::SidList::new();
    let mut current_file: usize = 0;

    let (mut len, mut playing_fallback) = if usb_ready {
        fat::list_sids(&msc, &mut file_list);
        info!("USB: {} .SID files in root", file_list.len());
        if !file_list.is_empty() {
            match fat::load_sid(&msc, 0, tune_buf) {
                Ok(n)  => { info!("USB: loaded {} bytes", n); (n, false) },
                Err(_) => {
                    info!("USB load failed — using built-in tune");
                    file_list.clear();
                    tune_buf[..FALLBACK_SID.len()].copy_from_slice(FALLBACK_SID);
                    (FALLBACK_SID.len(), true)
                }
            }
        } else {
            info!("No .SID on USB — using built-in tune");
            tune_buf[..FALLBACK_SID.len()].copy_from_slice(FALLBACK_SID);
            (FALLBACK_SID.len(), true)
        }
    } else {
        info!("No USB drive — using built-in tune");
        tune_buf[..FALLBACK_SID.len()].copy_from_slice(FALLBACK_SID);
        (FALLBACK_SID.len(), true)
    };
    info!("Loaded {} bytes", len);

    let mut hdr = psid::PsidHeader::parse(&tune_buf[..len]).expect("bad PSID");
    info!("PSID v{}: songs={} start={} init={:#x} play={:#x} speed={:#010x}",
          hdr.version, hdr.songs, hdr.start_song, hdr.init_addr, hdr.play_addr, hdr.speed);

    let mut current_subtune: u16 = hdr.start_song; // 1-based

    // -----------------------------------------------------------------
    // Step 2+3: Write the tune image and start playback.
    // -----------------------------------------------------------------
    let play_timer = peripherals.PLAY_TIMER;
    load_and_start(tune_buf, len, &mut hdr, current_subtune, &play_timer, false);
    info!("playback started");

    // -----------------------------------------------------------------
    // Step 4: Main loop — controls + display.
    // -----------------------------------------------------------------
    let mut encoder = Encoder0::new(peripherals.ENCODER0);
    let mut paused   = false;
    let mut redraw   = true; // draw immediately on first iteration
    let mut selected: usize = 0;     // 0=File, 1=Song, 2=State
    let mut modify   = false;        // "modifying" the focused item
    let mut browse_idx: usize = 0;   // highlighted file while modifying File

    loop {
        encoder.update();

        // -- Hot-plug: if playing fallback and USB drive appears, enumerate + load index 0 --
        if playing_fallback && msc.ready() {
            file_list.clear();
            fat::list_sids(&msc, &mut file_list);
            if !file_list.is_empty() {
                if let Ok(n) = fat::load_sid(&msc, 0, tune_buf) {
                    info!("Hot-plug: loaded {} bytes from USB", n);
                    len = n;
                    current_file = 0;
                    current_subtune = psid::PsidHeader::parse(&tune_buf[..len])
                        .map(|h| h.start_song).unwrap_or(1);
                    paused = false;
                    playing_fallback = false;
                    load_and_start(tune_buf, len, &mut hdr, current_subtune, &play_timer, false);
                    redraw = true;
                }
            }
        }

        // -- Encoder rotation --
        let ticks = encoder.poke_ticks();
        if ticks != 0 {
            if !modify {
                selected = (selected as i16 + ticks as i16).clamp(0, 2) as usize;
            } else {
                match selected {
                    0 => { // File: move browse cursor only — NO load
                        if !file_list.is_empty() {
                            browse_idx = (browse_idx as i16 + ticks as i16)
                                .clamp(0, file_list.len() as i16 - 1) as usize;
                        }
                    }
                    1 => { // Song: change subtune live
                        if hdr.songs > 1 {
                            current_subtune = (current_subtune as i16 + ticks as i16)
                                .clamp(1, hdr.songs as i16) as u16;
                            load_and_start(tune_buf, len, &mut hdr,
                                           current_subtune, &play_timer, paused);
                        }
                    }
                    _ => {}
                }
            }
            redraw = true;
        }

        // -- Encoder button --
        if encoder.poke_btn() {
            match selected {
                0 => { // File
                    if !file_list.is_empty() {
                        if !modify {
                            modify = true;
                            browse_idx = current_file; // start browse at playing file
                        } else {
                            // Commit. Same file = cancel (no-op).
                            if browse_idx != current_file {
                                if let Ok(n) = fat::load_sid(&msc, browse_idx, tune_buf) {
                                    len = n;
                                    current_file = browse_idx;
                                    current_subtune = psid::PsidHeader::parse(&tune_buf[..len])
                                        .map(|h| h.start_song).unwrap_or(1);
                                    paused = false;
                                    load_and_start(tune_buf, len, &mut hdr,
                                                   current_subtune, &play_timer, paused);
                                }
                            }
                            modify = false;
                        }
                    }
                }
                1 => { // Song
                    modify = !modify;
                }
                2 => { // State: toggle pause
                    paused = !paused;
                    let rate = hdr.is_ntsc(current_subtune);
                    if paused {
                        play_timer.control().write(|w| {
                            w.reset().clear_bit().play_rate().bit(rate).irq_enable().clear_bit()
                        });
                    } else {
                        play_timer.control().write(|w| {
                            w.reset().clear_bit().play_rate().bit(rate).irq_enable().set_bit()
                        });
                    }
                }
                _ => {}
            }
            redraw = true;
        }

        // -- Header text --
        // On a state change, clear the whole header band (full width) to wipe
        // stale text. The text itself is redrawn *every* loop so the persist
        // decay pass never fades the song details to black.
        if redraw {
            redraw = false;
            Rectangle::new(Point::new(0, 0), Size::new(h_active as u32, HEADER_H as u32))
                .into_styled(PrimitiveStyle::with_fill(HI8::BLACK))
                .draw(&mut display)
                .ok();
        }

        let name_str   = trim_ascii(&tune_buf[0x16..0x36]);
        let author_str = trim_ascii(&tune_buf[0x36..0x56]);

        // Line 1: title + tune name.
        let mut line1: String<80> = String::new();
        write!(line1, "SID PLAYER  {}", name_str).ok();
        Text::new(line1.as_str(), Point::new(20, 18), style)
            .draw(&mut display)
            .ok();

        // Line 2: author + song / state.
        let mut line2: String<96> = String::new();
        write!(line2, "{}   Song {}/{} [{}]",
               author_str, current_subtune, hdr.songs,
               if paused { "PAUSED" } else { "PLAYING" }).ok();
        Text::new(line2.as_str(), Point::new(20, 40), style_dim)
            .draw(&mut display)
            .ok();
    }
}
