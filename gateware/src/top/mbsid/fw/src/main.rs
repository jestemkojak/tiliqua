// main.rs — MINIMAL, THROWAWAY link-prover for the MBSID engine (Task 4 / M1
// milestone 2). It exists to (a) force libmbsid.a to link into the firmware ELF
// and (b) drive a real tone path: init engine -> load a Lead patch -> note_on ->
// per-tick register diff -> SIDPeripheral writes -> reSID -> codec.
//
// NO interrupt, NO MIDI input, NO UI/display — that is Task 5, which REPLACES this
// file with the Timer0-ISR + MIDI-in version (mirroring top/sid/fw/src/main.rs).
// The control loop here is a plain busy-delay at ~1 ms (the engine's control rate).

#![no_std]
#![no_main]

use riscv_rt::entry;
use panic_halt as _;

use tiliqua_pac as pac;
use tiliqua_hal as hal;

use tiliqua_fw::mbsid_sys;
use tiliqua_fw::regdiff::{RegDiff, WriteList};
use tiliqua_fw::patch::PATCH;

use hal::hal::delay::DelayNs;

// Generates Serial0/Timer0/etc. for this (binary-only) crate. Kept out of lib.rs
// so the host `cargo test --lib` build stays pure-Rust (no pac/hal on x86_64).
hal::impl_tiliqua_soc_pac!();

/// Write one (reg,val) to the SID peripheral, respecting FIFO backpressure
/// (poll txn_status.writable). Mirrors top/sid's `(data<<5)|addr` encoding.
fn sid_write(sid: &pac::SID_PERIPH, reg: u8, val: u8) {
    while !sid.txn_status().read().writable().bit() {}
    sid.transaction_data().write(|w| unsafe {
        w.transaction_data().bits(((val as u16) << 5) | (reg as u16))
    });
}

#[entry]
fn main() -> ! {
    let peripherals = pac::Peripherals::take().unwrap();
    let sysclk = pac::clock::sysclk();
    let mut timer = Timer0::new(peripherals.TIMER0, sysclk);
    let sid = peripherals.SID_PERIPH;

    // Bring up the engine and load the Lead patch, then sound a couple of notes
    // so milestone-2 scope verification has a tone to look at.
    mbsid_sys::init();
    mbsid_sys::load_patch(&PATCH);
    mbsid_sys::note_on(60, 100); // middle C
    mbsid_sys::note_on(64, 100); // E (held chord; Lead is mono but proves the path)

    let mut diff = RegDiff::new();
    let mut wl = WriteList::new();

    loop {
        // ~1 ms control rate (the engine's tick cadence). No ISR by design.
        timer.delay_ns(1_000_000);

        if mbsid_sys::tick() {
            diff.update(mbsid_sys::regs_l(), &mut wl);
            for (reg, val) in wl.iter() {
                sid_write(&sid, *reg, *val);
            }
            wl.clear();
        }
    }
}
