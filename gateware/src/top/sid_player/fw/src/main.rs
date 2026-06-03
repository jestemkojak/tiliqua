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

/// Extract a null-terminated ASCII string from a fixed-width byte slice.
fn trim_ascii(s: &[u8]) -> &str {
    let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
    core::str::from_utf8(&s[..end]).unwrap_or("?")
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

    // -----------------------------------------------------------------
    // Step 1: Show banner, wait for USB drive, load tune.
    // -----------------------------------------------------------------
    let style     = MonoTextStyle::new(&FONT_9X15_BOLD, HI8::new(0, 0xB));
    let style_dim = MonoTextStyle::new(&FONT_9X15_BOLD, HI8::new(0, 0x7));

    Text::new("SID PLAYER", Point::new(20, 20), style)
        .draw(&mut display)
        .ok();
    Text::new("Waiting for USB drive...", Point::new(20, 50), style_dim)
        .draw(&mut display)
        .ok();

    let msc = UsbMsc::new(peripherals.USB_MSC);
    msc.wait_ready();

    info!("USB MSC ready — loading .SID file");

    // Scratch buffer in PSRAM at +7 MB (well away from framebuffer at 0x20000000).
    let tune_buf: &mut [u8] = unsafe {
        core::slice::from_raw_parts_mut((PSRAM_BASE + 0x700000) as *mut u8, 65536)
    };

    let len = fat::load_first_sid(&msc, tune_buf).expect("no .SID file found");
    info!("Loaded {} bytes", len);

    let hdr = psid::PsidHeader::parse(&tune_buf[..len]).expect("bad PSID");
    info!("PSID v{}: songs={} start={} init={:#x} play={:#x}",
          hdr.version, hdr.songs, hdr.start_song, hdr.init_addr, hdr.play_addr);

    // -----------------------------------------------------------------
    // Step 2: Write tune payload to the 6502 PSRAM region.
    // -----------------------------------------------------------------
    let payload_raw = &tune_buf[hdr.data_offset as usize..len];
    let load_addr   = hdr.effective_load_addr(payload_raw);
    // If load_addr was embedded in payload (hdr.load_addr == 0), skip the 2 addr bytes.
    let payload = if hdr.load_addr == 0 { &payload_raw[2..] } else { payload_raw };
    write_6502_mem(CPU6502_PSRAM_BASE, load_addr, payload);

    let mut current_subtune: u16 = hdr.start_song; // 1-based
    let sub0 = (current_subtune.saturating_sub(1)) as u8;

    write_6502_mem(CPU6502_PSRAM_BASE, bootstrap::INIT_STUB_ADDR,
                   &bootstrap::init_stub(sub0, hdr.init_addr));
    write_6502_mem(CPU6502_PSRAM_BASE, bootstrap::NMI_STUB_ADDR,
                   &bootstrap::nmi_stub(hdr.play_addr));
    write_6502_mem(CPU6502_PSRAM_BASE, 0xFFFA, &bootstrap::vectors());

    // -----------------------------------------------------------------
    // Step 3: Start playback.
    // -----------------------------------------------------------------
    let play_timer = peripherals.PLAY_TIMER;
    let ntsc = hdr.is_ntsc(current_subtune);

    // Hold 6502 in reset, set play rate, disable irq.
    play_timer.control().write(|w| {
        w.reset().set_bit().play_rate().bit(ntsc).irq_enable().clear_bit()
    });
    // Release reset.
    play_timer.control().write(|w| {
        w.reset().clear_bit().play_rate().bit(ntsc).irq_enable().clear_bit()
    });
    // Allow init stub to run (~2M nops at 60 MHz ≈ 33 ms).
    for _ in 0..2_000_000u32 { unsafe { core::arch::asm!("nop"); } }
    // Enable NMI play ticks.
    play_timer.control().write(|w| {
        w.reset().clear_bit().play_rate().bit(ntsc).irq_enable().set_bit()
    });

    // -----------------------------------------------------------------
    // Step 4: Main loop — controls + display.
    // -----------------------------------------------------------------
    let mut encoder = Encoder0::new(peripherals.ENCODER0);
    let mut paused  = false;
    let mut redraw  = true; // draw immediately on first iteration

    loop {
        encoder.update();

        // -- Button: toggle pause --
        if encoder.poke_btn() {
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
            redraw = true;
        }

        // -- Encoder rotation: change subtune --
        let ticks = encoder.poke_ticks();
        if ticks != 0 && hdr.songs > 1 {
            current_subtune = (current_subtune as i16 + ticks as i16)
                .max(1)
                .min(hdr.songs as i16) as u16;

            let rate = hdr.is_ntsc(current_subtune);
            let s0   = (current_subtune - 1) as u8;

            // Rewrite init stub for new subtune.
            write_6502_mem(CPU6502_PSRAM_BASE, bootstrap::INIT_STUB_ADDR,
                           &bootstrap::init_stub(s0, hdr.init_addr));
            write_6502_mem(CPU6502_PSRAM_BASE, bootstrap::NMI_STUB_ADDR,
                           &bootstrap::nmi_stub(hdr.play_addr));

            // Hold 6502 in reset, switch rate.
            play_timer.control().write(|w| {
                w.reset().set_bit().play_rate().bit(rate).irq_enable().clear_bit()
            });
            for _ in 0..500_000u32 { unsafe { core::arch::asm!("nop"); } }
            // Release reset (init runs).
            play_timer.control().write(|w| {
                w.reset().clear_bit().play_rate().bit(rate).irq_enable().clear_bit()
            });
            for _ in 0..2_000_000u32 { unsafe { core::arch::asm!("nop"); } }
            // Re-enable play ticks unless paused.
            if !paused {
                play_timer.control().write(|w| {
                    w.reset().clear_bit().play_rate().bit(rate).irq_enable().set_bit()
                });
            }

            redraw = true;
        }

        // -- Redraw display when state changed --
        if redraw {
            redraw = false;

            display.clear(HI8::BLACK).ok();

            // Header.
            Text::new("SID PLAYER", Point::new(20, 20), style)
                .draw(&mut display)
                .ok();

            // Metadata from PSID header (offsets per C64 PSID spec).
            let name_str      = trim_ascii(&tune_buf[0x16..0x36]);
            let author_str    = trim_ascii(&tune_buf[0x36..0x56]);
            let copyright_str = trim_ascii(&tune_buf[0x56..0x76]);

            Text::new(name_str, Point::new(20, 40), style)
                .draw(&mut display)
                .ok();
            Text::new(author_str, Point::new(20, 60), style_dim)
                .draw(&mut display)
                .ok();
            Text::new(copyright_str, Point::new(20, 80), style_dim)
                .draw(&mut display)
                .ok();

            // Status line.
            let mut status: String<64> = String::new();
            write!(status, "Song: {} / {}  [{}]",
                   current_subtune, hdr.songs,
                   if paused { "PAUSED" } else { "PLAYING" }).ok();

            Text::new(status.as_str(), Point::new(20, 110), style)
                .draw(&mut display)
                .ok();
        }
    }
}
