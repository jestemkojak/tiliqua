#![no_std]
#![no_main]

use core::fmt::Write as FmtWrite;

use log::info;
use riscv_rt::entry;

use mos6502::cpu::CPU;
use mos6502::instruction::Nmos6502;

use tiliqua_pac as pac;
use tiliqua_fw::{cvmod, fat, player, psid, usb_msc::UsbMsc};
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

/// Snapshot the PSID name ($16) + author ($36) fields (32 bytes each) of the
/// tune currently in `tune_buf` into owned strings. The title line is painted
/// from these, NOT live from `tune_buf`: a failed load clobbers `tune_buf` with
/// the rejected file's bytes while the old tune keeps playing, so reading live
/// would garble the title/author until the next good load.
fn snapshot_meta(tune_buf: &[u8], name: &mut String<32>, author: &mut String<32>) {
    name.clear();   let _ = name.push_str(trim_ascii(&tune_buf[0x16..0x36]));
    author.clear(); let _ = author.push_str(trim_ascii(&tune_buf[0x36..0x56]));
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
    // NOTE: raw write primitive — no FIFO backpressure here. Bursty callers must
    // go through sid_write_bp (polls `writable`) instead: play_tick drains each
    // PLAY frame via sid_write_bp, and sid_reset/drain_sid_writes likewise, so
    // their >16-write bursts can't overflow the depth-16 FIFO. Call sid_write
    // directly only where a burst can't exceed the FIFO depth.
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
    for w in bus.writes.iter() {
        sid_write_bp(w.reg, w.val);
    }
    bus.writes.clear();
}

/// One backpressured SID write: poll the FIFO's `writable` before pushing
/// (bounded: a hardware fault degrades to a dropped write, not a hang).
fn sid_write_bp(reg: u8, val: u8) {
    let p = unsafe { pac::Peripherals::steal() };
    let mut spins = 0u32;
    while p.SID_PERIPH.txn_status().read().writable().bit_is_clear() {
        spins += 1;
        if spins >= 100_000 { break; }
    }
    sid_write(reg, val);
}

/// Reset the SID to its power-on state. PSID tunes assume a freshly reset
/// chip: Commando's INIT writes only the three gates + volume, and its frame-0
/// gate-ons then play whatever waveform/freq/sustain the *previous* tune or
/// run left behind — an audible stale-register noise burst at tune start that
/// a real C64 doesn't have. Run between image load and INIT on every (re)load.
///
/// Two steps, because register clears alone cannot reach oscillator state:
/// 1. TEST bit on all voices — zeroes each oscillator's phase accumulator and
///    resets the noise LFSR. Ring-mod / hard-sync voices (Commando's intro:
///    ctrl $15/$43) shape their output from the *neighbour* oscillator's
///    phase, so matching a fresh chip needs accumulators at 0, not just
///    registers. TEST stays set while the clears below drain (~1µs/write) and
///    is released by the zero pass reaching each CTRL register.
/// 2. $00 to all 25 registers, ascending (each voice's CTRL is zeroed before
///    its SR, so anything sounding is gated off into the fastest release).
/// 28 writes exceed the depth-16 transaction FIFO -> backpressured writes.
fn sid_reset() {
    for v in 0..3u8 {
        sid_write_bp(4 + v * 7, 0x08);
    }
    for reg in 0..=0x18u8 {
        sid_write_bp(reg, 0);
    }
}

/// Playback state driven by the TIMER0 interrupt at the tune's play rate.
struct Playback {
    cpu: PlayerCpu,
    play_addr: u16,
    paused: bool,
    shadow: cvmod::SidShadow,
    cv: cvmod::CvMod,
}

static PLAYBACK: Mutex<RefCell<Option<Playback>>> = Mutex::new(RefCell::new(None));

/// Free-running play-frame counter since boot (ISR-incremented; wraps). One
/// increment per PLAY frame in the TIMER0 ISR. No longer read by the UI loop
/// (the per-frame menu repaint that used it was removed); kept as a counter.
static PLAY_TICKS: AtomicU32 = AtomicU32::new(0);

/// Set the play rate: program the TIMER0 reload (`period` sync cycles; the timer
/// is a down-counter). Called on tune/subtune (re)load.
fn set_play_period(timer: &mut Timer0, period: u32) {
    timer.set_timeout_ticks(period);
}

/// TIMER0 ISR body: run one PLAY frame on the software 6502. Real-time work
/// lives here (not the UI loop) so menu redraws can never starve the audio.
fn play_tick() {
    // Count every tick (even while paused — the timer keeps firing). load/store,
    // not fetch_add: riscv32im has no atomic RMW; single-writer (this ISR).
    PLAY_TICKS.store(PLAY_TICKS.load(Ordering::Relaxed).wrapping_add(1), Ordering::Relaxed);
    critical_section::with(|cs| {
        let mut g = PLAYBACK.borrow_ref_mut(cs);
        if let Some(pb) = g.as_mut() {
            if !pb.paused && pb.play_addr != 0 {
                let _ = player::call(&mut pb.cpu, pb.play_addr, 2_000_000);
                // Drain this frame's captured writes to the SID, backpressured so
                // a >16-write burst cannot overflow the depth-16 transaction FIFO.
                // The FIFO's 1-per-phi2 (~1MHz) drain provides the inter-write
                // spacing; we no longer busy-wait to a fixed per-frame anchor. The
                // ADSR-phase jitter that anchor guarded against was disproven (0
                // affected notes at ±1ms), and the spin wasted the real-time budget
                // that 200Hz tunes need. See docs/superpowers/specs/
                // 2026-06-15-remove-paced-replay-anchor-design.md.
                //
                // Mirror each write into the shadow and record which registers the
                // tune touched this frame (dirty mask) for CV change-detection.
                let mut dirty: u32 = 0;
                for w in pb.cpu.memory.writes.iter() {
                    sid_write_bp(w.reg, w.val);
                    pb.shadow[(w.reg & 0x1F) as usize] = w.val;
                    dirty |= 1 << (w.reg & 0x1F);
                }
                // Read the 3 CV inputs + jack-detect, then apply CV modulation
                // on top of the tune's writes (override wins until the next
                // tune write to the same register).
                let p = unsafe { pac::Peripherals::steal() };
                let cv_raw = [
                    p.PMOD0_PERIPH.sample_i0().read().bits() as i16 as i32,
                    p.PMOD0_PERIPH.sample_i1().read().bits() as i16 as i32,
                    p.PMOD0_PERIPH.sample_i2().read().bits() as i16 as i32,
                ];
                let jacks = p.PMOD0_PERIPH.jack().read().bits();
                let writes = pb.cv.compute(&pb.shadow, dirty, cv_raw, jacks);
                for (reg, val) in writes.iter() {
                    sid_write_bp(*reg, *val);
                }
            }
        }
    });
}

/// Drive the gateware phi2 divider (0 = PAL 985.5kHz, 1 = NTSC 1.023MHz).
/// Like the scope CSRs this register is independent of the SID ISR state, so
/// no critical section is needed; the worst race with reload_tune is
/// last-write-wins of two writes derived from the same header.
fn set_phi2(clock: psid::Clock) {
    let ntsc = clock == psid::Clock::Ntsc;
    unsafe { (*pac::SID_PERIPH::ptr()).phi2_sel().write(|w| w.sel().bit(ntsc)) };
}

/// Effective SID clock standard for the Clock menu row:
/// 0 = AUTO (follow the PSID header), 1 = force PAL, 2 = force NTSC.
fn effective_clock(clock_sel: usize, hdr: &psid::PsidHeader) -> psid::Clock {
    match clock_sel {
        1 => psid::Clock::Pal,
        2 => psid::Clock::Ntsc,
        _ => hdr.clock(),
    }
}

/// Load a tune+subtune into the shared CPU and run INIT, under a critical
/// section so it can't race the timer ISR. Returns Some((play_period_cycles,
/// play_hz)) on success; None (leaving the current tune untouched) if the file
/// is unsupported/corrupt. The caller must update the TIMER0 reload on Some.
fn reload_tune(tune_buf: &[u8], len: usize, hdr: &mut psid::PsidHeader,
               subtune: u16, clock_sel: usize) -> Option<(u32, u32)> {
    let mut period: Option<u64> = None;
    critical_section::with(|cs| {
        let mut g = PLAYBACK.borrow_ref_mut(cs);
        let pb = g.as_mut().unwrap();
        if load_psid_to_mem(tune_buf, len, hdr, pb.cpu.memory.mem).is_err() {
            return; // leave `period` None -> caller treats as unsupported
        }
        // Only after the load is known-good: an unsupported file must leave
        // the still-playing current tune's SID state untouched.
        sid_reset();
        pb.shadow = [0; cvmod::SID_REGS];
        pb.cv.reset();
        pb.cpu.registers.stack_pointer.0 = 0xFD;
        player::init(&mut pb.cpu, hdr.init_addr, subtune.saturating_sub(1) as u8, 2_000_000);
        // Mirror INIT-time SID writes into the shadow so CV offsets start from
        // the tune's real post-INIT register values (drain_sid_writes clears them).
        for w in pb.cpu.memory.writes.iter() {
            pb.shadow[(w.reg & 0x1F) as usize] = w.val;
        }
        drain_sid_writes(&mut pb.cpu.memory); // INIT setup (volume/filter) -> SID now
        let cia = (pb.cpu.memory.mem[0xDC04] as u16) | ((pb.cpu.memory.mem[0xDC05] as u16) << 8);
        period = Some(psid::play_period_cycles(
            CLOCK_SYNC_HZ, hdr.clock(), hdr.is_cia(subtune), cia) as u64);
        pb.play_addr = hdr.play_addr;
        pb.paused = false;
    });
    if period.is_some() {
        // Successful load: retune the SID phi2 to this tune's standard
        // (or the forced override). Pitch follows the same header source
        // that already drives tempo.
        set_phi2(effective_clock(clock_sel, hdr));
    }
    period.map(|p| (p as u32, (CLOCK_SYNC_HZ as u64 / p) as u32))
}

/// Top-level menu card. Row 0 of every card is the "Page" selector.
#[derive(Clone, Copy, PartialEq)]
enum Page { Player, Config }

/// Row count per card, including the "Page" row at index 0.
fn rows_in(page: Page) -> usize {
    match page { Page::Player => 5, Page::Config => 7 }
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
    // Room for the 7-row Config card above the waveform. MUST match
    // persist_freeze_rows in top.py (the gateware freezes rows < this value
    // from phosphor decay so the menu doesn't flicker) — keep the two in sync.
    const HEADER_H: i16 = 200;

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

    static FALLBACK_SID: &[u8] = include_bytes!("../cracktro_vblank.sid");

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
    // frame's SID writes for backpressured replay (see play_tick).
    let image: &'static mut [u8; 0x10000] =
        unsafe { &mut *(0x2080_0000 as *mut [u8; 0x10000]) };
    let mut cpu: PlayerCpu =
        CPU::new(player::PsidBus { mem: image, writes: heapless::Vec::new(), dropped: 0 }, Nmos6502);
    cpu.registers.stack_pointer.0 = 0xFD;

    // Load initial tune and run INIT (hdr already validated/parsed above).
    // sid_reset is redundant right after bitstream load (the gateware holds SID
    // reset for the first 24 phi2 edges) but kept for uniformity with
    // reload_tune — it also covers warm relaunches from the bootloader.
    let _ = load_psid_to_mem(tune_buf, len, &mut hdr, cpu.memory.mem);
    sid_reset();
    player::init(&mut cpu, hdr.init_addr, (current_subtune.saturating_sub(1)) as u8, 2_000_000);
    drain_sid_writes(&mut cpu.memory); // INIT setup (volume/filter) -> SID now
    let cia = (cpu.memory.mem[0xDC04] as u16) | ((cpu.memory.mem[0xDC05] as u16) << 8);
    let period = psid::play_period_cycles(CLOCK_SYNC_HZ, hdr.clock(), hdr.is_cia(current_subtune), cia);
    info!("play rate: clock={:?} cia={} timer={:#x} period={} ({} Hz)",
          hdr.clock(), hdr.is_cia(current_subtune), cia, period, CLOCK_SYNC_HZ / period);
    let mut play_hz = CLOCK_SYNC_HZ / period;
    let mut play_period = period;
    set_phi2(hdr.clock()); // boot = AUTO: match the initial tune's standard

    // Hand the initialised CPU to the shared, ISR-visible playback state.
    critical_section::with(|cs| {
        PLAYBACK.borrow_ref_mut(cs).replace(Playback {
            cpu, play_addr: hdr.play_addr, paused: false,
            shadow: [0; cvmod::SID_REGS],
            cv: cvmod::CvMod::new(),
        });
    });

    // Real-time playback runs in the TIMER0 interrupt at the tune's exact rate
    // (reload = play_period sys-clk cycles). The UI loop below is best-effort:
    // it repaints the menu on input only (the menu band is frozen from persist
    // decay in gateware), while the scope region keeps decaying — too slow to
    // also host play() from this loop, hence the ISR.
    let mut timer = Timer0::new(peripherals.TIMER0, CLOCK_SYNC_HZ);
    let mut encoder = Encoder0::new(peripherals.ENCODER0);
    let mut paused   = false;
    // Title/author of the *playing* tune, snapshotted on each good load so a
    // later failed load (which clobbers tune_buf) can't garble them.
    let mut cur_name:   String<32> = String::new();
    let mut cur_author: String<32> = String::new();
    snapshot_meta(tune_buf, &mut cur_name, &mut cur_author);
    let mut unsupported = false; // last file selection was an unsupported .SID
    let mut redraw   = true;          // redraw all menu rows (page switch / refresh)
    let mut redraw_title = false;     // also clear title/author (tune load: name changed)
    let mut first_paint  = true;      // one-time full clear to wipe the boot splash
    let mut redraw_row: Option<usize> = None; // cheap single-row clear (one value edited)
    let mut page     = Page::Player;
    let mut selected: usize = 0;
    let mut modify   = false;
    let mut browse_idx: usize = 0;
    // Hot-plug enumeration latch: once a drive's files are listed we attempt a
    // single load. Without this the block re-lists + re-loads file 0 every loop
    // iteration while `playing_fallback` stays set (e.g. file 0 is unsupported),
    // spamming the log and starving audio with USB/PSRAM traffic. Reset when the
    // drive goes away so re-insertion is detected; left false while the list is
    // empty so the boot-time read race keeps retrying until files appear.
    let mut usb_listed = false;
    let mut prev_usb_present = false; // edge-detect USB plug/unplug for the Config row

    // Scope-card state (mirrors the initial scope/persist config above).
    let mut decay: u8     = 10;   // persistence 1..80
    let mut tb_idx: usize = 7;   // TIMEBASES index -> 2ms/d
    let mut ys_idx: usize = 2;   // VSCALES index   -> 2V/d
    let mut intensity: u8 = 8;   // 0..15
    let mut hue: u8       = 0;   // 0..15
    // Player-card Clock row: 0=AUTO (follow PSID header), 1=PAL, 2=NTSC.
    let mut clock_sel: usize = 0;

    handler!(timer0 = || play_tick());
    irq::scope(|s| {
        s.register(tiliqua_fw::handlers::Interrupt::TIMER0, timer0);
        // Boot grace period: when starting on the built-in tune (no USB drive
        // present yet), hold silent for ~1s before the play ISR begins. Mute
        // across the wait so the post-INIT chip state can't make a sound; the
        // delay is a OneShot busy-wait on TIMER0, which is fine here since the
        // tick ISR isn't armed yet. USB-loaded tunes start immediately.
        if playing_fallback {
            use embedded_hal::delay::DelayNs;
            output_mute(true);
            timer.delay_ms(1000);
            output_mute(false);
        }
        // enable_tick_isr sets periodic mode + listen + enables interrupts;
        // then override the reload with the cycle-accurate play period.
        timer.enable_tick_isr(20, pac::Interrupt::TIMER0);
        set_play_period(&mut timer, play_period);

        loop {
            encoder.update();

            // -- Hot-plug --
            // Same 512-byte guard as the initial mount (silent ignore: the
            // fallback tune keeps playing). `usb_listed` latches a single
            // enumeration+load attempt per insertion (see its declaration);
            // a drive going away re-arms it.
            let usb_present = msc.ready() && msc.block_size() == 512;
            if !usb_present { usb_listed = false; }
            // USB presence is shown live on the Config "Rescan USB" row
            // ("N files" / "NO DRIVE"). With the per-frame repaint gone, a
            // plug/unplug sets no other dirty flag, so refresh once on the edge.
            if usb_present != prev_usb_present {
                prev_usb_present = usb_present;
                redraw = true;
            }
            if playing_fallback && usb_present && !usb_listed {
                file_list.clear();
                fat::list_sids(&msc, &mut file_list);
                if !file_list.is_empty() {
                    usb_listed = true; // enumerated: don't re-list/re-load every frame
                    if let Ok(n) = fat::load_sid(&msc, 0, tune_buf) {
                        info!("Hot-plug: loaded {} bytes from USB", n);
                        let start = psid::PsidHeader::parse(&tune_buf[..n])
                            .map(|h| h.start_song).unwrap_or(1);
                        if let Some((p, hz)) = reload_tune(tune_buf, n, &mut hdr, start, clock_sel) {
                            len = n; current_file = 0; current_subtune = start;
                            paused = false; playing_fallback = false; unsupported = false;
                            output_mute(false);
                            play_period = p; play_hz = hz;
                            set_play_period(&mut timer, play_period);
                            redraw_title = true; // new tune: name/author changed
                            snapshot_meta(tune_buf, &mut cur_name, &mut cur_author);
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
                                Page::Config
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
                                    reload_tune(tune_buf, len, &mut hdr, current_subtune, clock_sel) {
                                    paused = false; output_mute(false);
                                    play_period = p; play_hz = hz;
                                    set_play_period(&mut timer, play_period);
                                }
                            }
                        }
                        (Page::Player, 3) => {
                            clock_sel = (clock_sel as i16 + ticks as i16)
                                .clamp(0, 2) as usize;
                            set_phi2(effective_clock(clock_sel, &hdr));
                        }
                        (Page::Config, 1) => {
                            decay = (decay as i16 + ticks as i16).clamp(1, 80) as u8;
                            persist.set_persistence(decay);
                        }
                        (Page::Config, 2) => {
                            tb_idx = (tb_idx as i16 + ticks as i16)
                                .clamp(0, TIMEBASES.len() as i16 - 1) as usize;
                            scope.set_timebase(TIMEBASES[tb_idx]);
                        }
                        (Page::Config, 3) => {
                            ys_idx = (ys_idx as i16 + ticks as i16)
                                .clamp(0, VSCALES.len() as i16 - 1) as usize;
                            scope.set_yscale(VSCALES[ys_idx]);
                        }
                        (Page::Config, 4) => {
                            intensity = (intensity as i16 + ticks as i16).clamp(0, 15) as u8;
                            scope.set_intensity(intensity);
                        }
                        (Page::Config, 5) => {
                            hue = (hue as i16 + ticks as i16).clamp(0, 15) as u8;
                            scope.set_hue(hue);
                        }
                        // Row 6 (Rescan USB) is an action triggered by press, not
                        // a value; rotation in modify does nothing (falls through).
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
                                            reload_tune(tune_buf, n, &mut hdr, start, clock_sel) {
                                            len = n; current_file = browse_idx;
                                            current_subtune = start;
                                            paused = false; unsupported = false;
                                            output_mute(false);
                                            play_period = p; play_hz = hz;
                                            set_play_period(&mut timer, play_period);
                                            // New tune: name/author/meta + every
                                            // row change -> clear title + rows.
                                            redraw_title = true;
                                            snapshot_meta(tune_buf, &mut cur_name, &mut cur_author);
                                        } else {
                                            // Unsupported file: keep playing the
                                            // current tune, flag it in the UI.
                                            // The State row (4) flips to
                                            // UNSUPPORTED! while `selected` is the
                                            // File row (1), so a single-row clear
                                            // would leave the old State text under
                                            // it (band is frozen). Redraw all rows.
                                            unsupported = true;
                                            redraw = true;
                                        }
                                    }
                                }
                                modify = false;
                            }
                        }
                    }
                    (Page::Player, 2) => { modify = !modify; }
                    (Page::Player, 3) => { modify = !modify; }
                    (Page::Player, 4) => {
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
                    // Rescan USB: re-enumerate the drive and load its first .SID
                    // (mirrors the boot/hot-plug path) so a freshly-swapped drive
                    // is picked up without a reboot. Fires on press directly (it's
                    // an action, not a value), so it never enters `modify`.
                    (Page::Config, 6) => {
                        if msc.ready() && msc.block_size() == 512 {
                            file_list.clear();
                            fat::list_sids(&msc, &mut file_list);
                            // Re-arm the auto hot-plug latch to match the new list:
                            // latched (no auto re-list) if we found files, else
                            // re-armed so the auto path keeps retrying.
                            usb_listed = !file_list.is_empty();
                            browse_idx = 0;
                            if let Ok(n) = fat::load_sid(&msc, 0, tune_buf) {
                                let start = psid::PsidHeader::parse(&tune_buf[..n])
                                    .map(|h| h.start_song).unwrap_or(1);
                                if let Some((p, hz)) =
                                    reload_tune(tune_buf, n, &mut hdr, start, clock_sel) {
                                    len = n; current_file = 0; current_subtune = start;
                                    paused = false; playing_fallback = false; unsupported = false;
                                    output_mute(false);
                                    play_period = p; play_hz = hz;
                                    set_play_period(&mut timer, play_period);
                                    redraw_title = true; // new tune: name changed
                                    snapshot_meta(tune_buf, &mut cur_name, &mut cur_author);
                                } else {
                                    // First file unsupported: keep current tune.
                                    unsupported = true;
                                }
                            }
                        }
                        // File list + every row may change -> redraw rows.
                        redraw = true;
                    }
                    // All other Config rows (scope params): press toggles modify,
                    // then rotate adjusts.
                    (Page::Config, _) => { modify = !modify; }
                    _ => {}
                }
                // Most button actions toggle a marker or one row's text; clear
                // just that row. Loading a new tune (above) sets redraw/title.
                if !redraw && !redraw_title { redraw_row = Some(selected); }
            }

            // The menu band (y < HEADER_H) is frozen from persist phosphor
            // decay in gateware (persist_freeze_rows, see top.py), so the text
            // persists without re-blitting. Repaint only on an actual change:
            // encoder rotate/press, or a pending clear (redraw / redraw_row,
            // set by every value/page/tune change above). This keeps the UI
            // loop off the PSRAM bus between interactions, leaving bandwidth
            // for the 6502's tune fetches (audio > visuals).
            if !(ticks != 0 || btn || redraw || redraw_title || redraw_row.is_some()) {
                continue;
            }

            // Menu text below is re-blitted only when the gate above passes
            // (input or a pending clear). Navigation (rotate without modify)
            // passes via `ticks != 0` and re-blits each row's font (selected vs
            // dim) over identical glyphs, so it needs no clear. Clears erase
            // ghosts when text shrinks (long filename -> short, "PLAYING" ->
            // "PAUSED"); they are per-pixel `draw_iter` fills (no accelerated
            // fill_solid in the HAL), slow under PSRAM contention — so we clear
            // as little as possible and NEVER the whole header at once:
            //   `first_paint`  -> one full clear to wipe the boot splash.
            //   `redraw`       -> each row band, cleared just-in-time in the
            //                     draw loop (row-by-row, no global blank) + the
            //                     taller card's trailing rows. NARROW centred
            //                     strip (rows live near cx), not full width.
            //   `redraw_title` -> additionally the full-width title/author bands
            //                     (only a tune load changes the 32-char name).
            //   `redraw_row`   -> one row's band (a single value was edited).
            let vy0    = 72i32;
            let vspace = 18i32;
            let cx     = h_active as i32 / 2;
            // Row text spans labels at cx-100 .. values right-aligned at cx+100
            // (+ marker), and the centred metadata line reaches ~cx±105; a
            // centred strip covers both without the full-width clear cost.
            let band_x = cx - 130;
            let band_w = 290u32;
            let first  = first_paint;            // wipe the boot splash, once
            let title  = redraw_title;           // tune load: name/author changed
            let rows   = redraw || redraw_title; // page switch / refresh / tune load
            first_paint  = false;
            redraw_title = false;
            redraw       = false;
            let single = if first || rows { redraw_row = None; None }
                         else { redraw_row.take() };
            if first {
                // Full one-time clear: the boot splash is left-aligned (x=20),
                // outside the centred strip, so the whole header must be wiped.
                Rectangle::new(Point::new(0, 0), Size::new(h_active as u32, HEADER_H as u32))
                    .into_styled(PrimitiveStyle::with_fill(HI8::BLACK)).draw(&mut display).ok();
            } else {
                if title {
                    // Title/author are centred and can be wide (32-char PSID
                    // name) -> full width.
                    for &ty in &[34i32, 54i32] {
                        Rectangle::new(Point::new(0, ty - 15), Size::new(h_active as u32, 20))
                            .into_styled(PrimitiveStyle::with_fill(HI8::BLACK)).draw(&mut display).ok();
                    }
                }
                if rows {
                    // Trailing rows of the taller (Config) card, plus the Player
                    // metadata line (y=162 == Config row 5). Current rows are
                    // cleared in the draw loop below.
                    for n in rows_in(page)..rows_in(Page::Config) {
                        let y = vy0 + vspace * n as i32;
                        Rectangle::new(Point::new(band_x, y - 15), Size::new(band_w, 20))
                            .into_styled(PrimitiveStyle::with_fill(HI8::BLACK)).draw(&mut display).ok();
                    }
                } else if let Some(row) = single {
                    // Single edited row. The Player "Song" row (2) also drives
                    // the metadata line at y=162, so extend the band to it.
                    let y   = vy0 + vspace * row as i32;
                    let bot = if page == Page::Player && row == 2 { 167 } else { y + 5 };
                    Rectangle::new(Point::new(band_x, y - 15), Size::new(band_w, (bot - (y - 15)) as u32))
                        .into_styled(PrimitiveStyle::with_fill(HI8::BLACK)).draw(&mut display).ok();
                }
            }

            // Painted from the snapshot, not live tune_buf (which a failed load
            // clobbers with the rejected file while the old tune plays on).
            let mut line1: String<80> = String::new();
            write!(line1, "SID PLAYER ({})  {}", build_model_str, cur_name.as_str()).ok();
            Text::with_alignment(line1.as_str(), Point::new(cx, 34), style, Alignment::Center)
                .draw(&mut display).ok();
            Text::with_alignment(cur_author.as_str(), Point::new(cx, 54), style_dim, Alignment::Center)
                .draw(&mut display).ok();

            let label_x  = cx - 100;
            let value_x  = cx + 100;
            let marker_x = cx + 110;

            for n in 0..rows_in(page) {
                let font = if selected == n { style } else { style_dim };
                let y = vy0 + vspace * n as i32;
                // Full row redraw: clear this row's band just before drawing it
                // (the boot-splash full clear already blanked everything on the
                // first paint). Row-by-row keeps the menu from blanking at once.
                if rows && !first {
                    Rectangle::new(Point::new(band_x, y - 15), Size::new(band_w, 20))
                        .into_styled(PrimitiveStyle::with_fill(HI8::BLACK)).draw(&mut display).ok();
                }
                let label = match (page, n) {
                    (_, 0)            => "Menu",
                    (Page::Player, 1) => "File",
                    (Page::Player, 2) => "Song",
                    (Page::Player, 3) => "Clock",
                    (Page::Player, _) => "State",
                    (Page::Config, 1) => "Decay",
                    (Page::Config, 2) => "Timebase",
                    (Page::Config, 3) => "Y-Scale",
                    (Page::Config, 4) => "Intensity",
                    (Page::Config, 6) => "Rescan USB",
                    (Page::Config, _) => "Hue",
                };
                let mut value: String<24> = String::new();
                match (page, n) {
                    (_, 0) => { write!(value, "{}", if page == Page::Player { "Player" } else { "Config" }).ok(); }
                    (Page::Player, 1) => {
                        let shown = if modify && selected == 1 { browse_idx } else { current_file };
                        let mark  = if !file_list.is_empty() && shown == current_file { "*" } else { "" };
                        let fname = file_list.get(shown).map(|s| s.as_str()).unwrap_or("<builtin>");
                        write!(value, "{}{}", mark, fname).ok();
                    }
                    (Page::Player, 2) => { write!(value, "{}/{}", current_subtune, hdr.songs).ok(); }
                    (Page::Player, 3) => {
                        match clock_sel {
                            1 => { write!(value, "PAL").ok(); }
                            2 => { write!(value, "NTSC").ok(); }
                            _ => {
                                let c = match hdr.clock() {
                                    psid::Clock::Ntsc => "NTSC",
                                    psid::Clock::Pal  => "PAL",
                                };
                                write!(value, "AUTO ({})", c).ok();
                            }
                        }
                    }
                    (Page::Player, _) => {
                        let state = if unsupported { "UNSUPPORTED!" }
                                    else if paused { "PAUSED" } else { "PLAYING" };
                        write!(value, "{}", state).ok();
                    }
                    (Page::Config, 1) => { write!(value, "{}", decay).ok(); }
                    (Page::Config, 2) => { let s: &str = TIMEBASES[tb_idx].into(); write!(value, "{}", s).ok(); }
                    (Page::Config, 3) => { let s: &str = VSCALES[ys_idx].into(); write!(value, "{}", s).ok(); }
                    (Page::Config, 4) => { write!(value, "{}", intensity).ok(); }
                    (Page::Config, 6) => {
                        // Reflects the last enumeration: file count, or NO DRIVE
                        // when nothing is mounted. Updates the moment Rescan runs.
                        if msc.ready() && msc.block_size() == 512 {
                            write!(value, "{} files", file_list.len()).ok();
                        } else {
                            write!(value, "NO DRIVE").ok();
                        }
                    }
                    (Page::Config, _) => { write!(value, "{}", hue).ok(); }
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
                Text::with_alignment(meta.as_str(), Point::new(cx, 162),
                                     style_dim, Alignment::Center)
                    .draw(&mut display).ok();
            }
        }
    })
}
