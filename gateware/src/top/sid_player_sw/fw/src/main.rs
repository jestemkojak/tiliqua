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

use core::cell::RefCell;
use critical_section::Mutex;
use irq::handler;

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

/// 6502 with a concrete (non-capturing) SID-write hook, so the whole CPU is a
/// nameable type and can live in a `static` shared with the timer ISR.
type PlayerCpu = CPU<player::PsidBus<fn(u8, u8)>, Nmos6502>;

/// Write one SID register via the SIDPeripheral CSR. Non-capturing so it works
/// as a plain `fn` pointer; steals SID_PERIPH (effectively a single owner).
fn sid_write(reg: u8, val: u8) {
    let p = unsafe { pac::Peripherals::steal() };
    p.SID_PERIPH.transaction_data().write(|w| unsafe {
        w.transaction_data().bits(player::sid_txn(reg, val))
    });
}

/// Playback state driven by the TIMER0 interrupt at the tune's play rate.
struct Playback {
    cpu: PlayerCpu,
    play_addr: u16,
    paused: bool,
}

static PLAYBACK: Mutex<RefCell<Option<Playback>>> = Mutex::new(RefCell::new(None));

/// TIMER0 ISR body: run one PLAY frame on the software 6502. Real-time work
/// lives here (not the UI loop) so menu redraws can never starve the audio.
fn play_tick() {
    critical_section::with(|cs| {
        let mut g = PLAYBACK.borrow_ref_mut(cs);
        if let Some(pb) = g.as_mut() {
            if !pb.paused && pb.play_addr != 0 {
                player::call(&mut pb.cpu, pb.play_addr, 2_000_000);
            }
        }
    });
}

/// Load a tune+subtune into the shared CPU and run INIT, under a critical
/// section so it can't race the timer ISR. Returns (play_period_cycles,
/// play_hz); the caller must update the TIMER0 reload to the new period.
fn reload_tune(tune_buf: &[u8], len: usize, hdr: &mut psid::PsidHeader,
               subtune: u16) -> (u32, u32) {
    let mut period: u64 = 1;
    critical_section::with(|cs| {
        let mut g = PLAYBACK.borrow_ref_mut(cs);
        let pb = g.as_mut().unwrap();
        load_psid_to_mem(tune_buf, len, hdr, pb.cpu.memory.mem);
        pb.cpu.registers.stack_pointer.0 = 0xFD;
        player::init(&mut pb.cpu, hdr.init_addr, subtune.saturating_sub(1) as u8, 2_000_000);
        let cia = (pb.cpu.memory.mem[0xDC04] as u16) | ((pb.cpu.memory.mem[0xDC05] as u16) << 8);
        period = psid::play_period_cycles(CLOCK_SYNC_HZ, hdr.clock(), hdr.is_cia(subtune), cia) as u64;
        pb.play_addr = hdr.play_addr;
        pb.paused = false;
    });
    (period as u32, (CLOCK_SYNC_HZ as u64 / period) as u32)
}

/// Top-level menu card. Row 0 of every card is the "Page" selector.
#[derive(Clone, Copy, PartialEq)]
enum Page { Player, Scope }

/// Row count per card, including the "Page" row at index 0.
fn rows_in(page: Page) -> usize {
    match page { Page::Player => 4, Page::Scope => 6 }
}

/// Selectable scope timebases / vertical scales (display label via IntoStaticStr).
const TIMEBASES: [Timebase; 13] = [
    Timebase::Timebase500ms, Timebase::Timebase200ms, Timebase::Timebase100ms,
    Timebase::Timebase50ms,  Timebase::Timebase20ms,  Timebase::Timebase10ms,
    Timebase::Timebase5ms,   Timebase::Timebase2ms,   Timebase::Timebase1ms,
    Timebase::Timebase500us, Timebase::Timebase200us, Timebase::Timebase100us,
    Timebase::Timebase50us,
];
const VSCALES: [VScale; 8] = [
    VScale::Scale8V,    VScale::Scale4V,   VScale::Scale2V,   VScale::Scale1V,
    VScale::Scale500mV, VScale::Scale250mV, VScale::Scale125mV, VScale::Scale64mV,
];

#[entry]
fn main() -> ! {
    let peripherals = pac::Peripherals::take().unwrap();
    let serial = Serial0::new(peripherals.UART0);
    let build_model_str = if unsafe { (*pac::SID_PERIPH::ptr()).build_model().read().model().bit() }
        { "8580" } else { "6581" };

    tiliqua_fw::handlers::logger_init(serial);
    // The mos6502 crate emits a `debug!` line per emulated instruction; at the
    // logger's default Trace level that floods the UART and (because each line
    // blocks on the slow UART) throttles playback to a crawl. Cap at Info.
    unsafe { log::set_max_level_racy(log::LevelFilter::Info); }
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
    const HEADER_H: i16 = 190; // room for the 6-row Scope card above the waveform

    let mut scope   = Scope0::new(peripherals.SCOPE_PERIPH, 6);
    let mut persist = Persist0::new(peripherals.PERSIST_PERIPH);

    // Minimum persistence: fastest decay (decay=15, holdoff=32, no skip) so the
    // waveform is crisp with a negligible phosphor trail. Persist is still the
    // framebuffer's clear mechanism for the additive-blended scope, so it can't
    // be disabled outright without the traces smearing to white.
    persist.set_persistence(1);
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
    // The RISC-V is the only master of this PSRAM window, so no cache thrashing
    // or coherency hacks are needed. The SID-write hook is a plain fn pointer
    // (not a capturing closure) so the CPU is a nameable type shareable with
    // the timer ISR.
    let image: &'static mut [u8; 0x10000] =
        unsafe { &mut *(0x2080_0000 as *mut [u8; 0x10000]) };
    let mut cpu: PlayerCpu =
        CPU::new(player::PsidBus { mem: image, on_sid_write: sid_write as fn(u8, u8) }, Nmos6502);
    cpu.registers.stack_pointer.0 = 0xFD;

    // Load initial tune and run INIT.
    load_psid_to_mem(tune_buf, len, &mut hdr, cpu.memory.mem);
    player::init(&mut cpu, hdr.init_addr, (current_subtune.saturating_sub(1)) as u8, 2_000_000);
    let cia = (cpu.memory.mem[0xDC04] as u16) | ((cpu.memory.mem[0xDC05] as u16) << 8);
    let period = psid::play_period_cycles(CLOCK_SYNC_HZ, hdr.clock(), hdr.is_cia(current_subtune), cia);
    info!("play rate: clock={:?} cia={} timer={:#x} period={} ({} Hz)",
          hdr.clock(), hdr.is_cia(current_subtune), cia, period, CLOCK_SYNC_HZ / period);
    let mut play_hz = CLOCK_SYNC_HZ / period;
    let mut play_period = period;

    // Hand the initialised CPU to the shared, ISR-visible playback state.
    critical_section::with(|cs| {
        PLAYBACK.borrow_ref_mut(cs).replace(Playback {
            cpu, play_addr: hdr.play_addr, paused: false,
        });
    });

    // Real-time playback runs in the TIMER0 interrupt at the tune's exact rate
    // (reload = play_period sys-clk cycles). The UI loop below is best-effort:
    // it must repaint the menu every frame (the persist/scope effect decays the
    // framebuffer), which is too slow to also host play() — hence the ISR.
    let mut timer = Timer0::new(peripherals.TIMER0, CLOCK_SYNC_HZ);
    let mut encoder = Encoder0::new(peripherals.ENCODER0);
    let mut paused   = false;
    let mut redraw   = true;
    let mut page     = Page::Player;
    let mut selected: usize = 0;
    let mut modify   = false;
    let mut browse_idx: usize = 0;

    // Scope-card state (mirrors the initial scope/persist config above).
    let mut decay: u8     = 10;   // persistence 1..80
    let mut tb_idx: usize = 7;   // TIMEBASES index -> 2ms/d
    let mut ys_idx: usize = 2;   // VSCALES index   -> 2V/d
    let mut intensity: u8 = 8;   // 0..15
    let mut hue: u8       = 0;   // 0..15

    handler!(timer0 = || play_tick());
    irq::scope(|s| {
        s.register(tiliqua_fw::handlers::Interrupt::TIMER0, timer0);
        // enable_tick_isr sets periodic mode + listen + enables interrupts;
        // then override the reload with the cycle-accurate play period.
        timer.enable_tick_isr(20, pac::Interrupt::TIMER0);
        timer.set_timeout_ticks(play_period);

        loop {
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
                        let (p, hz) = reload_tune(tune_buf, len, &mut hdr, current_subtune);
                        play_period = p; play_hz = hz;
                        timer.set_timeout_ticks(play_period);
                        redraw = true;
                    }
                }
            }

            let ticks = encoder.poke_ticks();
            if ticks != 0 {
                if !modify {
                    selected = (selected as i16 + ticks as i16)
                        .clamp(0, rows_in(page) as i16 - 1) as usize;
                } else {
                    match (page, selected) {
                        // Page row: switch card (2-value), then point at the Page row.
                        (_, 0) => {
                            let cur = if page == Page::Player { 0i16 } else { 1i16 };
                            page = if (cur + ticks as i16).clamp(0, 1) == 0 {
                                Page::Player
                            } else {
                                Page::Scope
                            };
                            selected = 0;
                        }
                        (Page::Player, 1) => {
                            if !file_list.is_empty() {
                                browse_idx = (browse_idx as i16 + ticks as i16)
                                    .clamp(0, file_list.len() as i16 - 1) as usize;
                            }
                        }
                        (Page::Player, 2) => {
                            if hdr.songs > 1 {
                                current_subtune = (current_subtune as i16 + ticks as i16)
                                    .clamp(1, hdr.songs as i16) as u16;
                                paused = false;
                                let (p, hz) = reload_tune(tune_buf, len, &mut hdr, current_subtune);
                                play_period = p; play_hz = hz;
                                timer.set_timeout_ticks(play_period);
                            }
                        }
                        (Page::Scope, 1) => {
                            decay = (decay as i16 + ticks as i16).clamp(1, 80) as u8;
                            persist.set_persistence(decay);
                        }
                        (Page::Scope, 2) => {
                            tb_idx = (tb_idx as i16 + ticks as i16)
                                .clamp(0, TIMEBASES.len() as i16 - 1) as usize;
                            scope.set_timebase(TIMEBASES[tb_idx]);
                        }
                        (Page::Scope, 3) => {
                            ys_idx = (ys_idx as i16 + ticks as i16)
                                .clamp(0, VSCALES.len() as i16 - 1) as usize;
                            scope.set_yscale(VSCALES[ys_idx]);
                        }
                        (Page::Scope, 4) => {
                            intensity = (intensity as i16 + ticks as i16).clamp(0, 15) as u8;
                            scope.set_intensity(intensity);
                        }
                        (Page::Scope, 5) => {
                            hue = (hue as i16 + ticks as i16).clamp(0, 15) as u8;
                            scope.set_hue(hue);
                        }
                        _ => {}
                    }
                }
                redraw = true;
            }

            if encoder.poke_btn() {
                match (page, selected) {
                    // Page row: enter/exit modify so rotation switches the card.
                    (_, 0) => { modify = !modify; }
                    (Page::Player, 1) => {
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
                                        let (p, hz) = reload_tune(tune_buf, len, &mut hdr, current_subtune);
                                        play_period = p; play_hz = hz;
                                        timer.set_timeout_ticks(play_period);
                                    }
                                }
                                modify = false;
                            }
                        }
                    }
                    (Page::Player, 2) => { modify = !modify; }
                    (Page::Player, 3) => {
                        paused = !paused;
                        critical_section::with(|cs| {
                            if let Some(pb) = PLAYBACK.borrow_ref_mut(cs).as_mut() {
                                pb.paused = paused;
                            }
                        });
                    }
                    // All Scope param rows: press toggles modify, then rotate adjusts.
                    (Page::Scope, _) => { modify = !modify; }
                    _ => {}
                }
                redraw = true;
            }

            // The menu must be repainted every frame: the persist/scope effect
            // continuously decays the framebuffer, so static text would fade.
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
            write!(line1, "SID PLAYER ({})  {}", build_model_str, name_str).ok();
            Text::with_alignment(line1.as_str(), Point::new(cx, 34), style, Alignment::Center)
                .draw(&mut display).ok();
            Text::with_alignment(author_str, Point::new(cx, 54), style_dim, Alignment::Center)
                .draw(&mut display).ok();

            let label_x  = cx - 100;
            let value_x  = cx + 100;
            let marker_x = cx + 110;
            let vy0      = 72i32;
            let vspace   = 18i32;

            for n in 0..rows_in(page) {
                let font = if selected == n { style } else { style_dim };
                let y = vy0 + vspace * n as i32;
                let label = match (page, n) {
                    (_, 0)            => "Menu",
                    (Page::Player, 1) => "File",
                    (Page::Player, 2) => "Song",
                    (Page::Player, _) => "State",
                    (Page::Scope, 1)  => "Decay",
                    (Page::Scope, 2)  => "Timebase",
                    (Page::Scope, 3)  => "Y-Scale",
                    (Page::Scope, 4)  => "Intensity",
                    (Page::Scope, _)  => "Hue",
                };
                let mut value: String<24> = String::new();
                match (page, n) {
                    (_, 0) => { write!(value, "{}", if page == Page::Player { "Player" } else { "Scope" }).ok(); }
                    (Page::Player, 1) => {
                        let shown = if modify && selected == 1 { browse_idx } else { current_file };
                        let mark  = if !file_list.is_empty() && shown == current_file { "*" } else { "" };
                        let fname = file_list.get(shown).map(|s| s.as_str()).unwrap_or("<builtin>");
                        write!(value, "{}{}", mark, fname).ok();
                    }
                    (Page::Player, 2) => { write!(value, "{}/{}", current_subtune, hdr.songs).ok(); }
                    (Page::Player, _) => { write!(value, "{}", if paused { "PAUSED" } else { "PLAYING" }).ok(); }
                    (Page::Scope, 1)  => { write!(value, "{}", decay).ok(); }
                    (Page::Scope, 2)  => { let s: &str = TIMEBASES[tb_idx].into(); write!(value, "{}", s).ok(); }
                    (Page::Scope, 3)  => { let s: &str = VSCALES[ys_idx].into(); write!(value, "{}", s).ok(); }
                    (Page::Scope, 4)  => { write!(value, "{}", intensity).ok(); }
                    (Page::Scope, _)  => { write!(value, "{}", hue).ok(); }
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

            // Tune metadata line — only relevant on the Player card.
            if page == Page::Player {
                let clock_str = match hdr.clock() { psid::Clock::Ntsc => "NTSC", psid::Clock::Pal => "PAL" };
                let speed_str = if hdr.is_cia(current_subtune) { "CIA" } else { "VBI" };
                let mut meta: String<40> = String::new();
                write!(meta, "{}  {}  {}  {} Hz",
                       hdr.model().as_str(), clock_str, speed_str, play_hz).ok();
                Text::with_alignment(meta.as_str(), Point::new(cx, 150),
                                     style_dim, Alignment::Center)
                    .draw(&mut display).ok();
            }
        }
    })
}
