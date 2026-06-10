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
use core::sync::atomic::{AtomicU32, Ordering};
use critical_section::Mutex;
use irq::handler;

/// Extract a null-terminated ASCII string from a fixed-width byte slice.
fn trim_ascii(s: &[u8]) -> &str {
    let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
    core::str::from_utf8(&s[..end]).unwrap_or("?")
}

/// Write the tune payload into the 6502 memory image and zero CIA Timer A.
/// Returns Err (without touching `mem`/`hdr`) for unsupported/corrupt files so
/// callers can skip them gracefully instead of crashing.
fn load_psid_to_mem(tune_buf: &[u8], len: usize, hdr: &mut psid::PsidHeader,
                    mem: &mut [u8; 0x10000]) -> Result<(), psid::PsidError> {
    *hdr = psid::PsidHeader::parse(&tune_buf[..len])?;
    let payload_raw = &tune_buf[hdr.data_offset as usize..len];
    let load_addr = hdr.effective_load_addr(payload_raw) as usize;
    let payload = if hdr.load_addr == 0 { &payload_raw[2..] } else { payload_raw };
    mem[load_addr..load_addr + payload.len()].copy_from_slice(payload);
    // Zero CIA #1 Timer A so we can detect if INIT programs it (multispeed).
    mem[0xDC04] = 0;
    mem[0xDC05] = 0;
    Ok(())
}

/// The non-generic `PsidBus` makes the whole CPU a nameable type, so it can
/// live in a `static` shared with the timer ISR.
type PlayerCpu = CPU<player::PsidBus, Nmos6502>;

/// Write one SID register via the SIDPeripheral CSR. Steals SID_PERIPH
/// (effectively a single owner).
fn sid_write(reg: u8, val: u8) {
    // NOTE: no FIFO backpressure here, and it still can't overflow. Replay paces
    // PLAY-frame writes at their real 1MHz cycle spacing (see play_tick), matching
    // the FIFO's 1-per-phi2 drain; same-stamp bursts are ≤16 deep. INIT bursts can
    // exceed depth-16, so drain_sid_writes adds its own backpressure.
    let p = unsafe { pac::Peripherals::steal() };
    p.SID_PERIPH.transaction_data().write(|w| unsafe {
        w.transaction_data().bits(player::sid_txn(reg, val))
    });
}

/// Mute/unmute the codec output. Used on pause to mask the SID's held notes
/// (the chip keeps oscillating its last state while play() is stopped) without
/// touching the SID itself, so playback resumes cleanly. Plain write (mirrors
/// the pmod HAL's `mute()`); other flag bits default to 0.
fn output_mute(mute: bool) {
    let p = unsafe { pac::Peripherals::steal() };
    p.PMOD0_PERIPH.flags().write(|w| w.mute().bit(mute));
}

/// Drain captured writes straight to the SID, then clear the buffer. Used after
/// INIT (one-shot setup: volume, filter) — tunes that set $D418 only in INIT
/// would otherwise be silent. INIT bursts can exceed the depth-16 FIFO (register
/// clears run 25+ writes back-to-back), so poll `writable` before each write;
/// bounded so a hardware fault can't wedge the caller (falls back to dropping).
fn drain_sid_writes(bus: &mut player::PsidBus) {
    let p = unsafe { pac::Peripherals::steal() };
    for w in bus.writes.iter() {
        let mut spins = 0u32;
        while p.SID_PERIPH.txn_status().read().writable().bit_is_clear() {
            spins += 1;
            if spins >= 100_000 { break; }
        }
        sid_write(w.reg, w.val);
    }
    bus.writes.clear();
}

/// Playback state driven by the TIMER0 interrupt at the tune's play rate.
struct Playback {
    cpu: PlayerCpu,
    play_addr: u16,
    paused: bool,
}

static PLAYBACK: Mutex<RefCell<Option<Playback>>> = Mutex::new(RefCell::new(None));

/// DIAGNOSTIC (running peaks/totals; UI loop emits over UART). Measures whether
/// the emulator runs faster or slower than a real 1MHz 6502 — the deciding factor
/// for whether cycle-accurate pacing is feasible:
///   PLAY_MAX_DUR  — worst PLAY-frame emulation wall-clock (sync/60MHz cycles,
///     emulation only — replay waits at a fixed anchor and is excluded).
///   PLAY_CPU_CYC  — 6502 cycles that same frame ran (cpu.cycles delta).
///     effective MHz = PLAY_CPU_CYC / (PLAY_MAX_DUR/60). <1 => slower than 1MHz.
///   PLAY_OVERRUN  — frames whose ISR (emulation+replay) exceeded the play
///     period (tick lost).
///   PLAY_CALL_BAD — `call()` overran max_steps (emulator stuck on an opcode).
///   PLAY_MIN_DUR  — best PLAY-frame emulation wall-clock; min/max spread =
///     the inter-frame jitter the fixed anchor exists to absorb.
///   PLAY_REPLAY_BAIL — frames whose replay crossed the period boundary (writes
///     issued unpaced from there on; relative spacing lost for that frame).
///   PLAY_LATE_ANCHOR — frames where emulation overran the fixed anchor offset
///     (replay fell back to anchoring at end-of-emulation: jitter for that
///     frame). Must be 0 for inter-frame timing to be deterministic.
///   PLAY_DROPS    — cumulative SID writes lost to a full capture buffer.
static PLAY_MAX_DUR:     AtomicU32 = AtomicU32::new(0);
static PLAY_MIN_DUR:     AtomicU32 = AtomicU32::new(u32::MAX);
static PLAY_CPU_CYC:     AtomicU32 = AtomicU32::new(0);
static PLAY_OVERRUN:     AtomicU32 = AtomicU32::new(0);
static PLAY_CALL_BAD:    AtomicU32 = AtomicU32::new(0);
static PLAY_REPLAY_BAIL: AtomicU32 = AtomicU32::new(0);
static PLAY_LATE_ANCHOR: AtomicU32 = AtomicU32::new(0);
static PLAY_DROPS:       AtomicU32 = AtomicU32::new(0);

/// Current play period in sync cycles (TIMER0 reload), ISR-visible: the replay
/// anchor offset is derived from it. Written via `set_play_period` only.
static PLAY_PERIOD: AtomicU32 = AtomicU32::new(0);

/// Play ticks since boot (ISR-incremented; wraps). The UI loop divides by
/// play_hz to pace repaints — blits are PSRAM traffic that competes with the
/// 6502's tune fetches, and the framebuffer only refreshes ~60 Hz.
static PLAY_TICKS: AtomicU32 = AtomicU32::new(0);

/// Update the play rate everywhere it matters: the TIMER0 reload and the
/// ISR-visible copy the replay anchor is derived from.
fn set_play_period(timer: &mut Timer0, period: u32) {
    PLAY_PERIOD.store(period, Ordering::Relaxed);
    timer.set_timeout_ticks(period);
}

/// TIMER0 ISR body: run one PLAY frame on the software 6502. Real-time work
/// lives here (not the UI loop) so menu redraws can never starve the audio.
fn play_tick() {
    // DIAGNOSTIC: time the emulated frame against the play period (Timer0 is a
    // periodic down-counter reloaded to play_period each tick).
    let timer = unsafe { Timer0::summon() };
    let c_start = timer.counter();
    // Count every tick (even while paused — the timer keeps firing) so the UI
    // loop can pace its repaints. load/store, not fetch_add: riscv32im has no
    // atomic RMW; single-writer (this ISR).
    PLAY_TICKS.store(PLAY_TICKS.load(Ordering::Relaxed).wrapping_add(1), Ordering::Relaxed);
    let (dcyc, c_mid): (u32, u32) = critical_section::with(|cs| {
        let mut g = PLAYBACK.borrow_ref_mut(cs);
        if let Some(pb) = g.as_mut() {
            if !pb.paused && pb.play_addr != 0 {
                let c0 = pb.cpu.cycles;
                if !player::call(&mut pb.cpu, pb.play_addr, 2_000_000) {
                    PLAY_CALL_BAD.store(PLAY_CALL_BAD.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
                }
                // Replay the captured frame at the SID's real 1MHz spacing: the
                // emulator stretched inter-write gaps ~5x, breaking envelopes.
                // Anchor at a FIXED offset from the tick (half the play period),
                // NOT at end-of-emulation: emulation duration swings by ~ms
                // frame-to-frame (workload + cache state), and an end-of-emulation
                // anchor passes that jitter into the *inter-frame* write spacing,
                // re-rolling the SID envelope (ADSR delay bug) phase at every
                // gate-on — a real C64 delivers frame-locked, deterministic
                // timing. Commando-class tunes trigger every note across frames
                // (gate-off 3+ frames before gate-on), so inter-frame timing is
                // the load-bearing part. If emulation already overran the offset,
                // fall back to anchoring here (counted; that frame jitters).
                // Timer0 is a down-counter; 1 emulated 6502 cycle = 60 sync
                // ticks; elapsed since anchor = t0 - c.
                let offset = PLAY_PERIOD.load(Ordering::Relaxed) / 2;
                let c_mid = timer.counter();
                let lead = c_start.wrapping_sub(c_mid); // emu time (huge if reloaded)
                let (t0, base) = if lead < offset {
                    (c_start, offset)
                } else {
                    PLAY_LATE_ANCHOR.store(
                        PLAY_LATE_ANCHOR.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
                    (c_mid, 0)
                };
                let mut bailed = false;
                for w in pb.cpu.memory.writes.iter() {
                    if !bailed {
                        let target = base + w.cycle * 60; // sync ticks from anchor
                        loop {
                            let c = timer.counter();
                            if c > t0 {
                                // Counter reloaded: we crossed the period boundary.
                                // t0 - c would underflow — bail: issue this and all
                                // remaining writes immediately (unpaced) so none are
                                // lost, and stop spinning for the rest of the frame.
                                PLAY_REPLAY_BAIL.store(
                                    PLAY_REPLAY_BAIL.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
                                bailed = true;
                                break;
                            }
                            if t0 - c >= target { break; }
                        }
                    }
                    sid_write(w.reg, w.val);
                }
                PLAY_DROPS.store(pb.cpu.memory.dropped, Ordering::Relaxed);
                return ((pb.cpu.cycles - c0) as u32, c_mid); // 6502 cycles + emu end
            }
        }
        (0, c_start)
    });
    let c_end = timer.counter();
    // Counter went UP => it hit 0 and reloaded mid-frame => the ISR took longer
    // than the play period (the next tick can be coalesced => a skipped PLAY
    // frame => a dropped note). Otherwise c_start - c_mid is this frame's
    // emulation time (replay's fixed-anchor wait excluded); pair it with the
    // 6502-cycle count to get the effective emulation MHz.
    if c_end > c_start {
        PLAY_OVERRUN.store(PLAY_OVERRUN.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
    } else if c_mid <= c_start {
        let dur = c_start - c_mid;
        if dur > PLAY_MAX_DUR.load(Ordering::Relaxed) {
            PLAY_MAX_DUR.store(dur, Ordering::Relaxed);
            PLAY_CPU_CYC.store(dcyc, Ordering::Relaxed);
        }
        // Min only over frames that actually ran PLAY — paused/idle ISR passes
        // would otherwise drive it to ~0 and hide the real jitter spread.
        if dcyc > 0 && dur < PLAY_MIN_DUR.load(Ordering::Relaxed) {
            PLAY_MIN_DUR.store(dur, Ordering::Relaxed);
        }
    }
}

/// Load a tune+subtune into the shared CPU and run INIT, under a critical
/// section so it can't race the timer ISR. Returns Some((play_period_cycles,
/// play_hz)) on success; None (leaving the current tune untouched) if the file
/// is unsupported/corrupt. The caller must update the TIMER0 reload on Some.
fn reload_tune(tune_buf: &[u8], len: usize, hdr: &mut psid::PsidHeader,
               subtune: u16) -> Option<(u32, u32)> {
    let mut period: Option<u64> = None;
    critical_section::with(|cs| {
        let mut g = PLAYBACK.borrow_ref_mut(cs);
        let pb = g.as_mut().unwrap();
        if load_psid_to_mem(tune_buf, len, hdr, pb.cpu.memory.mem).is_err() {
            return; // leave `period` None -> caller treats as unsupported
        }
        pb.cpu.registers.stack_pointer.0 = 0xFD;
        player::init(&mut pb.cpu, hdr.init_addr, subtune.saturating_sub(1) as u8, 2_000_000);
        drain_sid_writes(&mut pb.cpu.memory); // INIT setup (volume/filter) -> SID now
        let cia = (pb.cpu.memory.mem[0xDC04] as u16) | ((pb.cpu.memory.mem[0xDC05] as u16) << 8);
        period = Some(psid::play_period_cycles(
            CLOCK_SYNC_HZ, hdr.clock(), hdr.is_cia(subtune), cia) as u64);
        pb.play_addr = hdr.play_addr;
        pb.paused = false;
    });
    period.map(|p| (p as u32, (CLOCK_SYNC_HZ as u64 / p) as u32))
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
    persist.set_persistence(11);
    scope.set_intensity(8);
    scope.set_yscale(VScale::Scale4V);
    scope.set_xscale(7);
    scope.set_timebase(Timebase::Timebase2ms);
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

    // 512-byte blocks only: read_block drains a fixed 128 words and the
    // gateware byte packer never backpressures, so any other sector size
    // silently corrupts every read (§5c). Refuse the drive instead.
    let usb_ready = usb_ready && match msc.block_size() {
        512 => true,
        bs => {
            info!("USB: unsupported block size {} (need 512) — ignoring drive", bs);
            false
        }
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

    // If the first file is unsupported/corrupt, fall back to the built-in tune
    // rather than panicking (the built-in is always valid).
    let mut hdr = match psid::PsidHeader::parse(&tune_buf[..len]) {
        Ok(h) => h,
        Err(e) => {
            info!("Unsupported PSID ({:?}) — using built-in tune", e);
            tune_buf[..FALLBACK_SID.len()].copy_from_slice(FALLBACK_SID);
            len = FALLBACK_SID.len();
            playing_fallback = true;
            psid::PsidHeader::parse(&tune_buf[..len]).expect("built-in PSID is valid")
        }
    };
    info!("PSID v{}: songs={} start={} init={:#x} play={:#x} speed={:#010x}",
          hdr.version, hdr.songs, hdr.start_song, hdr.init_addr, hdr.play_addr, hdr.speed);

    let mut current_subtune: u16 = hdr.start_song; // 1-based

    // --- Construct software 6502 CPU over the 64KB PSRAM image -----------
    // The RISC-V is the only master of this PSRAM window, so no cache thrashing
    // or coherency hacks are needed. The non-generic PsidBus makes the CPU a
    // nameable type shareable with the timer ISR; its `writes` Vec captures each
    // frame's SID writes for paced replay (see play_tick).
    let image: &'static mut [u8; 0x10000] =
        unsafe { &mut *(0x2080_0000 as *mut [u8; 0x10000]) };
    let mut cpu: PlayerCpu =
        CPU::new(player::PsidBus { mem: image, writes: heapless::Vec::new(), dropped: 0 }, Nmos6502);
    cpu.registers.stack_pointer.0 = 0xFD;

    // Load initial tune and run INIT (hdr already validated/parsed above).
    let _ = load_psid_to_mem(tune_buf, len, &mut hdr, cpu.memory.mem);
    player::init(&mut cpu, hdr.init_addr, (current_subtune.saturating_sub(1)) as u8, 2_000_000);
    drain_sid_writes(&mut cpu.memory); // INIT setup (volume/filter) -> SID now
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
    let mut unsupported = false; // last file selection was an unsupported .SID
    let mut redraw   = true;          // full-header clear (page switch / tune load)
    let mut redraw_row: Option<usize> = None; // cheap single-row clear (one value edited)
    let mut page     = Page::Player;
    let mut selected: usize = 0;
    let mut modify   = false;
    let mut browse_idx: usize = 0;
    let mut dbg_div: u32 = 0; // DIAGNOSTIC: throttles the PLAY-timing UART line
    let mut last_paint_ticks: u32 = 0; // play-tick of the last menu repaint

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
        set_play_period(&mut timer, play_period);

        loop {
            encoder.update();

            // -- Hot-plug --
            // Same 512-byte guard as the initial mount (silent ignore: the
            // fallback tune keeps playing).
            if playing_fallback && msc.ready() && msc.block_size() == 512 {
                file_list.clear();
                fat::list_sids(&msc, &mut file_list);
                if !file_list.is_empty() {
                    if let Ok(n) = fat::load_sid(&msc, 0, tune_buf) {
                        info!("Hot-plug: loaded {} bytes from USB", n);
                        let start = psid::PsidHeader::parse(&tune_buf[..n])
                            .map(|h| h.start_song).unwrap_or(1);
                        if let Some((p, hz)) = reload_tune(tune_buf, n, &mut hdr, start) {
                            len = n; current_file = 0; current_subtune = start;
                            paused = false; playing_fallback = false; unsupported = false;
                            output_mute(false);
                            play_period = p; play_hz = hz;
                            set_play_period(&mut timer, play_period);
                        } else {
                            unsupported = true; // stay on the built-in tune
                        }
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
                                // Same (already-valid) tune, just a new subtune.
                                if let Some((p, hz)) =
                                    reload_tune(tune_buf, len, &mut hdr, current_subtune) {
                                    paused = false; output_mute(false);
                                    play_period = p; play_hz = hz;
                                    set_play_period(&mut timer, play_period);
                                }
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
                    // A value edit changes only its own row's text (the Song
                    // row also drives the metadata line), so clear just that
                    // band — a full per-pixel header clear visibly blanks the
                    // screen under PSRAM contention. A page switch (row 0)
                    // re-labels every row, so it still needs the full clear.
                    // Pure navigation (the `!modify` branch) sets neither: the
                    // every-frame text redraw handles the highlight change.
                    if selected == 0 { redraw = true; }
                    else { redraw_row = Some(selected); }
                }
            }

            let btn = encoder.poke_btn();
            if btn {
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
                                        let start = psid::PsidHeader::parse(&tune_buf[..n])
                                            .map(|h| h.start_song).unwrap_or(1);
                                        if let Some((p, hz)) =
                                            reload_tune(tune_buf, n, &mut hdr, start) {
                                            len = n; current_file = browse_idx;
                                            current_subtune = start;
                                            paused = false; unsupported = false;
                                            output_mute(false);
                                            play_period = p; play_hz = hz;
                                            set_play_period(&mut timer, play_period);
                                            // New tune: name/author/meta + every
                                            // row change at once -> full clear.
                                            redraw = true;
                                        } else {
                                            // Unsupported file: keep playing the
                                            // current tune, flag it in the UI.
                                            unsupported = true;
                                        }
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
                        // Mute the output while paused to mask the SID's held
                        // notes; unmute on resume. The SID keeps its state, so
                        // playback continues seamlessly.
                        output_mute(paused);
                    }
                    // All Scope param rows: press toggles modify, then rotate adjusts.
                    (Page::Scope, _) => { modify = !modify; }
                    _ => {}
                }
                // Most button actions toggle a marker or one row's text; clear
                // just that row. Loading a new tune (above) sets full `redraw`.
                if !redraw { redraw_row = Some(selected); }
            }

            // Pace repaints to ~the play rate (~50-60 Hz; the framebuffer
            // refresh rate): the loop otherwise free-runs and re-blits the whole
            // menu thousands of times/sec, and every blit is PSRAM traffic
            // competing with the 6502's tune fetches (audio > visuals — this is
            // what made Commando's dense "fast part" drop notes/stutter). Inputs
            // and pending clears force an immediate repaint so navigation never
            // lags; everything above this gate (encoder, hot-plug) is CSR-only
            // and stays per-iteration.
            let now_ticks = PLAY_TICKS.load(Ordering::Relaxed);
            let elapsed = now_ticks.wrapping_sub(last_paint_ticks);
            if !(ticks != 0 || btn || redraw || redraw_row.is_some()
                 || elapsed.saturating_mul(60) >= play_hz) {
                continue;
            }
            last_paint_ticks = now_ticks;

            // DIAGNOSTIC (throttled to the paint cadence so it can't flood the
            // blocking UART — that would throttle playback): dump the PLAY
            // frame-timing peaks/totals (cumulative, never reset). dur min..max =
            // emulation-time jitter the fixed anchor absorbs; drops/bail/late
            // must stay 0 for deterministic delivery.
            dbg_div = dbg_div.wrapping_add(1);
            if dbg_div % 64 == 0 {
                let dur  = PLAY_MAX_DUR.load(Ordering::Relaxed);
                let mn   = PLAY_MIN_DUR.load(Ordering::Relaxed);
                let cyc  = PLAY_CPU_CYC.load(Ordering::Relaxed);
                let ovr  = PLAY_OVERRUN.load(Ordering::Relaxed);
                let bad  = PLAY_CALL_BAD.load(Ordering::Relaxed);
                let drp  = PLAY_DROPS.load(Ordering::Relaxed);
                let bail = PLAY_REPLAY_BAIL.load(Ordering::Relaxed);
                let late = PLAY_LATE_ANCHOR.load(Ordering::Relaxed);
                // eff%: 6502 cycles vs the phi2 cycles (dur/60) that frame took.
                // 100 = exactly 1MHz; <100 = slower than a real 6502.
                let effpct = if dur != 0 { (cyc as u64 * 60 * 100 / dur as u64) as u32 } else { 0 };
                info!("PLAY dur={}..{} cpu_cyc={} eff={}% period={} overrun={} stuck={} drops={} bail={} late={}",
                      mn, dur, cyc, effpct, play_period, ovr, bad, drp, bail, late);
            }

            // Menu text is re-blitted every frame below (the persist/scope
            // effect decays the framebuffer), so navigation needs no clear.
            // Clears erase ghosts left when text shrinks (long filename ->
            // short, "PLAYING" -> "PAUSED"). They are per-pixel `draw_iter`
            // fills (no accelerated fill_solid in the HAL) that blank whatever
            // they cover for the fill's duration under PSRAM contention, so we
            // clear as little as possible:
            //   `redraw`     -> whole header (page switch / tune load: every row
            //                   plus name/author/meta change at once).
            //   `redraw_row` -> one row's band (a single value was edited).
            // Plain row navigation sets neither.
            let vy0    = 72i32;
            let vspace = 18i32;
            if redraw {
                redraw = false;
                redraw_row = None; // full clear supersedes any pending row clear
                Rectangle::new(Point::new(0, 0), Size::new(h_active as u32, HEADER_H as u32))
                    .into_styled(PrimitiveStyle::with_fill(HI8::BLACK))
                    .draw(&mut display)
                    .ok();
            } else if let Some(row) = redraw_row.take() {
                // Clear only the changed row's text band (full width: long
                // filenames span the line). The Player "Song" row (2) also
                // drives the metadata line at y=150, so extend the band to it.
                let y   = vy0 + vspace * row as i32;
                let top = y - 15;
                let bot = if page == Page::Player && row == 2 { 155 } else { y + 5 };
                Rectangle::new(Point::new(0, top),
                               Size::new(h_active as u32, (bot - top) as u32))
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
                    (Page::Player, _) => {
                        let state = if unsupported { "UNSUPPORTED!" }
                                    else if paused { "PAUSED" } else { "PLAYING" };
                        write!(value, "{}", state).ok();
                    }
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
