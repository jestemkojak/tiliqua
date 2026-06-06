#![no_std]
#![no_main]

use core::fmt::Write as FmtWrite;

use log::info;
use riscv_rt::entry;

use mos6502::cpu::CPU;
use mos6502::instruction::Nmos6502;

use tiliqua_pac as pac;
use tiliqua_fw::{fat, player, psid, usb_msc::UsbMsc};
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
    text::{Text, Alignment},
    geometry::Point,
};

use heapless::String;

/// Extract a null-terminated ASCII string from a fixed-width byte slice.
fn trim_ascii(s: &[u8]) -> &str {
    let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
    core::str::from_utf8(&s[..end]).unwrap_or("?")
}

/// Write the tune payload into the 6502 memory image and zero CIA Timer A.
fn load_psid_to_mem(tune_buf: &[u8], len: usize, hdr: &mut psid::PsidHeader,
                    mem: &mut [u8; 0x10000]) {
    *hdr = psid::PsidHeader::parse(&tune_buf[..len]).expect("bad PSID");
    let payload_raw = &tune_buf[hdr.data_offset as usize..len];
    let load_addr = hdr.effective_load_addr(payload_raw) as usize;
    let payload = if hdr.load_addr == 0 { &payload_raw[2..] } else { payload_raw };
    mem[load_addr..load_addr + payload.len()].copy_from_slice(payload);
    // Zero CIA #1 Timer A so we can detect if INIT programs it (multispeed).
    mem[0xDC04] = 0;
    mem[0xDC05] = 0;
}

#[entry]
fn main() -> ! {
    let peripherals = pac::Peripherals::take().unwrap();
    let serial = Serial0::new(peripherals.UART0);

    tiliqua_fw::handlers::logger_init(serial);
    info!("Hello from SID Player SW!");

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

    let h_active = display.size().width  as i16;
    let v_active = display.size().height as i16;
    const HEADER_H: i16 = 160;

    let mut scope   = Scope0::new(peripherals.SCOPE_PERIPH, 6);
    let mut persist = Persist0::new(peripherals.PERSIST_PERIPH);

    persist.set_persistence(10);
    scope.set_intensity(8);
    scope.set_yscale(VScale::Scale2V);
    scope.set_xscale(7);
    scope.set_timebase(Timebase::Timebase10ms);
    scope.set_trigger_level(0);
    scope.set_hue(0);
    scope.set_xpos_px(0);

    let centre = v_active / 2;
    for ch in 0..4i16 {
        let row = HEADER_H + ((ch * 2 + 1) * (v_active - HEADER_H)) / 8;
        scope.set_ypos_px(ch as usize, row - centre);
    }
    scope.set_enabled(true, true);

    static FALLBACK_SID: &[u8] = include_bytes!("../Gyroscope_3.sid");

    let style     = MonoTextStyle::new(&FONT_9X15_BOLD, HI8::new(0, 0xB));
    let style_dim = MonoTextStyle::new(&FONT_9X15_BOLD, HI8::new(0, 0x7));

    Text::new("SID PLAYER", Point::new(20, 20), style)
        .draw(&mut display).ok();
    Text::new("Insert USB drive, or plays built-in tune...", Point::new(20, 50), style_dim)
        .draw(&mut display).ok();

    let msc = UsbMsc::new(peripherals.USB_MSC);

    const USB_TIMEOUT: u32 = 120_000_000;
    let usb_ready = {
        let mut ready = false;
        for _ in 0..USB_TIMEOUT {
            if msc.ready() { ready = true; break; }
            unsafe { core::arch::asm!("nop"); }
        }
        ready
    };

    // Scratch buffer in PSRAM at +7 MB.
    let tune_buf: &mut [u8] = unsafe {
        core::slice::from_raw_parts_mut((PSRAM_BASE + 0x700000) as *mut u8, 65536)
    };

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

    // --- Construct software 6502 CPU over the 64KB PSRAM image -----------
    // The RISC-V is the only master of this PSRAM window, so no cache
    // thrashing or coherency hacks are needed.
    let image: &'static mut [u8; 0x10000] =
        unsafe { &mut *(0x2080_0000 as *mut [u8; 0x10000]) };
    let sid_periph = peripherals.SID_PERIPH;
    let on_sid = move |reg: u8, val: u8| {
        sid_periph.transaction_data().write(|w| unsafe {
            w.transaction_data().bits(player::sid_txn(reg, val))
        });
    };
    let mut cpu = CPU::new(player::PsidBus { mem: image, on_sid_write: on_sid }, Nmos6502);
    cpu.registers.stack_pointer.0 = 0xFD;

    // Load initial tune and run INIT.
    load_psid_to_mem(tune_buf, len, &mut hdr, cpu.memory.mem);
    info!("INIT: calling init={:#x} subtune={} (if no further log, INIT panicked/hung)",
          hdr.init_addr, current_subtune.saturating_sub(1));
    let init_ok = player::init(&mut cpu, hdr.init_addr, (current_subtune.saturating_sub(1)) as u8, 2_000_000);
    info!("INIT: {} pc={:#x} sp={:#x}",
          if init_ok { "returned (RTS)" } else { "OVERRAN max_steps" },
          cpu.registers.program_counter, cpu.registers.stack_pointer.0);
    let cia = (cpu.memory.mem[0xDC04] as u16) | ((cpu.memory.mem[0xDC05] as u16) << 8);
    let period = psid::play_period_cycles(CLOCK_SYNC_HZ, hdr.clock(), hdr.is_cia(current_subtune), cia) as u64;
    info!("play rate: clock={:?} cia={} timer={:#x} period={} ({} Hz)",
          hdr.clock(), hdr.is_cia(current_subtune), cia, period, CLOCK_SYNC_HZ as u64 / period);
    let mut play_hz = (CLOCK_SYNC_HZ as u64 / period) as u32;
    let mut play_period = period;

    // Playback clock: the gateware Timer0 free-running as a down-counter (this
    // SoC's VexiiRiscv has no `mcycle`/perf-counter CSR — reading it traps).
    // reload = u32::MAX so successive `last - now` deltas are exact modulo 2^32;
    // `acc` accumulates elapsed sys-clk cycles, firing play() every play_period.
    let mut timer = Timer0::new(peripherals.TIMER0, CLOCK_SYNC_HZ);
    timer.set_mode(tiliqua_hal::timer::Mode::Periodic);
    timer.set_timeout_ticks(u32::MAX);
    timer.enable();
    let mut last = timer.counter();
    let mut acc: u64 = 0;

    let mut encoder = Encoder0::new(peripherals.ENCODER0);
    let mut paused   = false;
    let mut redraw   = true;
    let mut selected: usize = 0;
    let mut modify   = false;
    let mut browse_idx: usize = 0;

    info!("entering main loop (menu should now be drawn)");
    let mut play_count: u32 = 0;

    loop {
        // --- Play tick: call play() once per period -----------------------
        // Accumulate elapsed sys-clk cycles from the free-running down-counter.
        let now = timer.counter();
        acc = acc.wrapping_add(last.wrapping_sub(now) as u64);
        last = now;
        if !paused && acc >= play_period {
            acc -= play_period;
            if hdr.play_addr != 0 {
                let t0 = timer.counter();
                let play_ok = player::call(&mut cpu, hdr.play_addr, 2_000_000);
                // Log the first few frames, then only overruns, to avoid flooding.
                if play_count < 3 || !play_ok {
                    info!("PLAY[{}]: {} cycles={} pc={:#x} sp={:#x}",
                          play_count,
                          if play_ok { "ok" } else { "OVERRAN" },
                          t0.wrapping_sub(timer.counter()),
                          cpu.registers.program_counter, cpu.registers.stack_pointer.0);
                }
                play_count = play_count.wrapping_add(1);
            }
        }

        encoder.update();

        // -- Hot-plug --
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
                    load_psid_to_mem(tune_buf, len, &mut hdr, cpu.memory.mem);
                    cpu.registers.stack_pointer.0 = 0xFD;
                    player::init(&mut cpu, hdr.init_addr, (current_subtune.saturating_sub(1)) as u8, 2_000_000);
                    let cia = (cpu.memory.mem[0xDC04] as u16) | ((cpu.memory.mem[0xDC05] as u16) << 8);
                    play_period = psid::play_period_cycles(CLOCK_SYNC_HZ, hdr.clock(), hdr.is_cia(current_subtune), cia) as u64;
                    play_hz = (CLOCK_SYNC_HZ as u64 / play_period) as u32;
                    acc = 0; last = timer.counter();
                    redraw = true;
                }
            }
        }

        let ticks = encoder.poke_ticks();
        if ticks != 0 {
            if !modify {
                selected = (selected as i16 + ticks as i16).clamp(0, 2) as usize;
            } else {
                match selected {
                    0 => {
                        if !file_list.is_empty() {
                            browse_idx = (browse_idx as i16 + ticks as i16)
                                .clamp(0, file_list.len() as i16 - 1) as usize;
                        }
                    }
                    1 => {
                        if hdr.songs > 1 {
                            current_subtune = (current_subtune as i16 + ticks as i16)
                                .clamp(1, hdr.songs as i16) as u16;
                            load_psid_to_mem(tune_buf, len, &mut hdr, cpu.memory.mem);
                            cpu.registers.stack_pointer.0 = 0xFD;
                            player::init(&mut cpu, hdr.init_addr, (current_subtune.saturating_sub(1)) as u8, 2_000_000);
                            let cia = (cpu.memory.mem[0xDC04] as u16) | ((cpu.memory.mem[0xDC05] as u16) << 8);
                            play_period = psid::play_period_cycles(CLOCK_SYNC_HZ, hdr.clock(), hdr.is_cia(current_subtune), cia) as u64;
                            play_hz = (CLOCK_SYNC_HZ as u64 / play_period) as u32;
                            acc = 0; last = timer.counter();
                        }
                    }
                    _ => {}
                }
            }
            redraw = true;
        }

        if encoder.poke_btn() {
            match selected {
                0 => {
                    if !file_list.is_empty() {
                        if !modify {
                            modify = true;
                            browse_idx = current_file;
                        } else {
                            if browse_idx != current_file {
                                if let Ok(n) = fat::load_sid(&msc, browse_idx, tune_buf) {
                                    len = n;
                                    current_file = browse_idx;
                                    current_subtune = psid::PsidHeader::parse(&tune_buf[..len])
                                        .map(|h| h.start_song).unwrap_or(1);
                                    paused = false;
                                    load_psid_to_mem(tune_buf, len, &mut hdr, cpu.memory.mem);
                                    cpu.registers.stack_pointer.0 = 0xFD;
                                    player::init(&mut cpu, hdr.init_addr, (current_subtune.saturating_sub(1)) as u8, 2_000_000);
                                    let cia = (cpu.memory.mem[0xDC04] as u16) | ((cpu.memory.mem[0xDC05] as u16) << 8);
                                    play_period = psid::play_period_cycles(CLOCK_SYNC_HZ, hdr.clock(), hdr.is_cia(current_subtune), cia) as u64;
                                    play_hz = (CLOCK_SYNC_HZ as u64 / play_period) as u32;
                                    acc = 0; last = timer.counter();
                                }
                            }
                            modify = false;
                        }
                    }
                }
                1 => { modify = !modify; }
                2 => { paused = !paused; }
                _ => {}
            }
            redraw = true;
        }

        if redraw {
            redraw = false;
            Rectangle::new(Point::new(0, 0), Size::new(h_active as u32, HEADER_H as u32))
                .into_styled(PrimitiveStyle::with_fill(HI8::BLACK))
                .draw(&mut display)
                .ok();
        }

        let name_str   = trim_ascii(&tune_buf[0x16..0x36]);
        let author_str = trim_ascii(&tune_buf[0x36..0x56]);

        let cx = h_active as i32 / 2;
        let mut line1: String<80> = String::new();
        write!(line1, "SID PLAYER  {}", name_str).ok();
        Text::with_alignment(line1.as_str(), Point::new(cx, 34), style, Alignment::Center)
            .draw(&mut display).ok();
        Text::with_alignment(author_str, Point::new(cx, 54), style_dim, Alignment::Center)
            .draw(&mut display).ok();

        let label_x  = cx - 100;
        let value_x  = cx + 100;
        let marker_x = cx + 110;
        let vy0      = 78i32;
        let vspace   = 20i32;

        for n in 0..3usize {
            let font = if selected == n { style } else { style_dim };
            let y = vy0 + vspace * n as i32;
            let label = match n { 0 => "File", 1 => "Song", _ => "State" };
            let mut value: String<24> = String::new();
            match n {
                0 => {
                    let shown = if modify && selected == 0 { browse_idx } else { current_file };
                    let mark  = if !file_list.is_empty() && shown == current_file { "*" } else { "" };
                    let fname = file_list.get(shown).map(|s| s.as_str()).unwrap_or("<builtin>");
                    write!(value, "{}{}", mark, fname).ok();
                }
                1 => { write!(value, "{}/{}", current_subtune, hdr.songs).ok(); }
                _ => { write!(value, "{}", if paused { "PAUSED" } else { "PLAYING" }).ok(); }
            }
            Text::new(label, Point::new(label_x, y), font)
                .draw(&mut display).ok();
            Text::with_alignment(value.as_str(), Point::new(value_x, y), font, Alignment::Right)
                .draw(&mut display).ok();
            if modify && selected == n {
                Text::new("<", Point::new(marker_x, y), font)
                    .draw(&mut display).ok();
            }
        }

        let clock_str = match hdr.clock() { psid::Clock::Ntsc => "NTSC", psid::Clock::Pal => "PAL" };
        let speed_str = if hdr.is_cia(current_subtune) { "CIA" } else { "VBI" };
        let mut meta: String<40> = String::new();
        write!(meta, "{}  {}  {} Hz", clock_str, speed_str, play_hz).ok();
        Text::with_alignment(meta.as_str(), Point::new(cx, 140),
                             style_dim, Alignment::Center)
            .draw(&mut display).ok();
    }
}
