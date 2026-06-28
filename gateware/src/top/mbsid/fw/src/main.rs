// main.rs — MBSID-on-Tiliqua M2 dual-SID firmware (Tasks 5–6).
//
// A 1 kHz Timer0 ISR is the whole engine. Each 1 ms tick it:
//   1. drains the SIDPeripheral MIDI-in CSR FIFO and feeds events to the MBSID
//      engine (note on/off, pitch bend, CC);
//   2. ticks the engine (`mbsid_tick`) at the 1 kHz control rate the host oracle
//      validated against;
//   3. diffs the L and R register images vs their 32-byte shadows and streams
//      only the changed `(data<<5)|addr` words to SIDPeripheral (L) and
//      SIDPeripheral_R (R) respectively (φ2 = 1 MHz reSID each).
//
// Derived from `top/sid/fw/src/main.rs` but stripped to the bone: NO menu/opts UI,
// NO CV modulation, NO per-note SID voice allocation (the engine owns voices),
// NO display/scope. All real-time work is in the ISR (the VexiiRiscv has no
// usable mcycle CSR; Timer0 is the only clock — see repo CLAUDE.md).

#![no_std]
#![no_main]

use critical_section::Mutex;
use core::cell::RefCell;

use riscv_rt::entry;
use irq::{handler, scoped_interrupts};
use amaranth_soc_isr::return_as_is;

use panic_halt as _;

use tiliqua_pac as pac;
use tiliqua_hal as hal;

use tiliqua_fw::mbsid_sys;
use tiliqua_fw::regdiff::{RegDiff, WriteList};

use midi_types::MidiMessage;
use midi_convert::parse::MidiTryParseSlice;

// Generates Serial0/Timer0/etc. for this (binary-only) crate. Kept out of lib.rs
// so the host `cargo test --lib` build stays pure-Rust (no pac/hal on x86_64).
hal::impl_tiliqua_soc_pac!();

// The engine's control rate is 1 kHz (DESIGN §8): the host oracle ticks the
// engine every 1 ms, so a 1 ms ISR is the only apples-to-apples cadence. (Base
// `top/sid` uses 5 ms — that is wrong for the engine; do not copy it.)
pub const TIMER0_ISR_PERIOD_MS: u32 = 1;

// Boot patch = factory bank slot loaded at power-on. 0-based slot index =
// MIDI Program Change value = (patch number - 1). 123 = A124 "Crazy Lead".
// MUST be a Lead-engine slot, or the synth boots with a wrong-sounding
// non-Lead patch (the 9 non-Lead slots are 15, 32-35, 60, 98, 99, 106).
const BOOT_PATCH_INDEX: u8 = 123;

// Scoped TIMER0 interrupt + its dispatch from riscv-rt's DefaultHandler. This is
// the minimal slice of `top/sid`'s handlers.rs we still need (no logger/UI).
scoped_interrupts! {
    #[allow(non_camel_case_types)]
    enum Interrupt {
        TIMER0,
    }
    use #[return_as_is];
}

#[export_name = "DefaultHandler"]
fn default_isr_handler() {
    let peripherals = unsafe { pac::Peripherals::steal() };
    let sysclk = pac::clock::sysclk();
    let timer = Timer0::new(peripherals.TIMER0, sysclk);
    if timer.is_pending() {
        unsafe { TIMER0(); }
        timer.clear_pending();
    }
}

/// Engine-adjacent ISR state: per-SID register-diff shadows (L/R) and the
/// shared scratch write list, shared with the Timer0 ISR via `static APP`.
struct App {
    diff_l: RegDiff,
    diff_r: RegDiff,
    wl:     WriteList,
}

impl App {
    fn new() -> Self {
        Self { diff_l: RegDiff::new(), diff_r: RegDiff::new(), wl: WriteList::new() }
    }
}

/// Drain a write list to a SID peripheral, respecting FIFO backpressure
/// (poll txn_status.writable). `(data<<5)|addr` encoding == `top/sid`.
/// Closures abstract over the two distinct PAC peripheral types.
fn drain_writelist(wl: &WriteList,
                   writable: impl Fn() -> bool,
                   write_word: impl Fn(u16)) {
    for (reg, val) in wl.iter() {
        while !writable() {}
        write_word(((*val as u16) << 5) | (*reg as u16));
    }
}

/// 1 kHz control ISR: MIDI in -> engine -> tick -> diff -> SID writes.
fn timer0_handler(app: &Mutex<RefCell<App>>) {
    let peripherals = unsafe { pac::Peripherals::steal() };
    let sid   = peripherals.SID_PERIPH;
    let sid_r = peripherals.SID_PERIPH_R;

    critical_section::with(|cs| {
        let mut app = app.borrow_ref_mut(cs);

        // (a) Drain the MIDI-in CSR FIFO (read until 0, as top/sid does) and
        //     dispatch each parsed message into the engine.
        loop {
            let word = sid.midi_read().read().bits();
            if word == 0 { break; }
            let bytes = [
                (word & 0xFF) as u8,
                ((word >> 8) & 0xFF) as u8,
                ((word >> 16) & 0xFF) as u8,
            ];
            if let Ok(msg) = MidiMessage::try_parse_slice(&bytes) {
                match msg {
                    // Note-on with non-zero velocity -> engine note on.
                    MidiMessage::NoteOn(_, note, vel) if u8::from(vel) > 0 => {
                        mbsid_sys::note_on(u8::from(note), u8::from(vel));
                    }
                    // Note-on vel 0 (running-status note-off) or explicit note-off.
                    MidiMessage::NoteOn(_, note, _) |
                    MidiMessage::NoteOff(_, note, _) => {
                        mbsid_sys::note_off(u8::from(note));
                    }
                    // Pitch bend: the engine wants the raw 14-bit MIDI value
                    // (msb<<7)|lsb, range 0..16383, center 8192 — exactly what
                    // MbSid.cpp reconstructs from the wire and feeds to
                    // midiReceivePitchBend(). We rebuild it from the raw data
                    // bytes (bytes[1]=LSB, bytes[2]=MSB) to avoid any signed/
                    // centered re-interpretation by midi-types' PitchBend type.
                    MidiMessage::PitchBendChange(_, _) => {
                        let lsb = (bytes[1] & 0x7F) as u16;
                        let msb = (bytes[2] & 0x7F) as u16;
                        mbsid_sys::pitch_bend((msb << 7) | lsb);
                    }
                    // Control change -> engine CC.
                    MidiMessage::ControlChange(_, ctrl, val) => {
                        mbsid_sys::cc(u8::from(ctrl), u8::from(val));
                    }
                    // Program Change -> load factory bank patch N (0..127) via
                    // the engine bankLoad path. Accepted on any MIDI channel.
                    MidiMessage::ProgramChange(_ch, prog) => {
                        mbsid_sys::program_change(u8::from(prog));
                    }
                    _ => {}
                }
            }
        }

        // (b) Tick the engine; on a register change, diff L and R vs their
        //     shadows and stream only the changed regs to their SIDs.
        if mbsid_sys::tick() {
            let App { diff_l, diff_r, wl } = &mut *app;

            diff_l.update(mbsid_sys::regs_l(), wl);
            drain_writelist(wl,
                || sid.txn_status().read().writable().bit(),
                |w| { sid.transaction_data().write(|r| unsafe { r.transaction_data().bits(w) }); });
            wl.clear();

            diff_r.update(mbsid_sys::regs_r(), wl);
            drain_writelist(wl,
                || sid_r.txn_status().read().writable().bit(),
                |w| { sid_r.transaction_data().write(|r| unsafe { r.transaction_data().bits(w) }); });
            wl.clear();
        }
    });
}

#[entry]
fn main() -> ! {
    let peripherals = pac::Peripherals::take().unwrap();
    let sysclk = pac::clock::sysclk();
    let mut timer = Timer0::new(peripherals.TIMER0, sysclk);

    // Bring up the engine and load the boot Lead patch so the SoC sounds from the
    // first MIDI note (patch loaded via factory bank at BOOT_PATCH_INDEX). Reset the diff shadow so the
    // first tick streams the full power-on register image.
    mbsid_sys::init();
    mbsid_sys::program_change(BOOT_PATCH_INDEX);

    let mut app = App::new();
    app.diff_l.reset();
    app.diff_r.reset();
    let app = Mutex::new(RefCell::new(app));

    // MIDI source: TRS-in by default (M1). The SIDPeripheral USB-MIDI host stays
    // disabled at reset, so no extra config is needed for the TRS path.

    handler!(timer0 = || timer0_handler(&app));

    irq::scope(|s| {
        s.register(Interrupt::TIMER0, timer0);
        timer.enable_tick_isr(TIMER0_ISR_PERIOD_MS, pac::Interrupt::TIMER0);

        // All work happens in the ISR; idle the core between ticks.
        loop {
            unsafe { riscv::asm::wfi(); }
        }
    })
}
