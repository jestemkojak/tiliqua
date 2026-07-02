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
use tiliqua_fw::sysex_capture::SysexCapture;
use tiliqua_fw::patch_store::{UserPatchStore, USER_BANK_FLASH_BASE};
use tiliqua_fw::menu::PressResult;

use midi_types::MidiMessage;
use midi_convert::parse::MidiTryParseSlice;

use tiliqua_lib::{bootinfo, palette};
use tiliqua_hal::encoder::Encoder;
use tiliqua_hal::persist::Persist;
use pac::constants::*;
use tiliqua_fw::menu::{self, MenuState};

// Generates Serial0/Timer0/etc. for this (binary-only) crate. Kept out of lib.rs
// so the host `cargo test --lib` build stays pure-Rust (no pac/hal on x86_64).
hal::impl_tiliqua_soc_pac!();

// The engine's control rate is 1 kHz (DESIGN §8): the host oracle ticks the
// engine every 1 ms, so a 1 ms ISR is the only apples-to-apples cadence. (Base
// `top/sid` uses 5 ms — that is wrong for the engine; do not copy it.)
pub const TIMER0_ISR_PERIOD_MS: u32 = 1;

// Max SysEx bytes drained per 1 ms tick: bounds ISR time. 32 B/ms = 32 kB/s
// drain >> 3.1 kB/s serial MIDI; USB is backpressured by the 64-deep gateware
// FIFO, so the cap costs only latency (a 1.6 kB dump ~ 50 ms), never data.
const SYSEX_BYTES_PER_TICK: u32 = 32;
// Abort a half-received SysEx message after this RX gap (spec §6a [DEFAULT]).
const SYSEX_TIMEOUT_MS: u16 = 500;

// Boot patch = factory bank slot loaded at power-on. 0-based slot index =
// MIDI Program Change value = (patch number - 1). 123 = A124 "Crazy Lead".
// MUST be a Lead-engine slot, or the synth boots with a wrong-sounding
// non-Lead patch (the 9 non-Lead slots are 15, 32-35, 60, 98, 99, 106).
const BOOT_PATCH_INDEX: u8 = 123;

const MENU_X: i32 = 60;
const MENU_Y: i32 = 80;
const MENU_HUE: u8 = 10;
const MENU_PERSIST: u8 = 80;

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
/// shared scratch write list, shared with the Timer0 ISR via `Mutex<RefCell<App>>`.
struct App {
    diff_l: RegDiff,
    diff_r: RegDiff,
    wl:     WriteList,
    sysex_cap: SysexCapture,
    sysex_idle_ms: u16,
    /// Complete Bank Write captured by the ISR; persisted by the main loop
    /// (flash I/O must never run in the 1 ms ISR).
    pending_save: Option<(u8, [u8; 512])>,
}

impl App {
    fn new() -> Self {
        Self {
            diff_l: RegDiff::new(), diff_r: RegDiff::new(), wl: WriteList::new(),
            sysex_cap: SysexCapture::new(), sysex_idle_ms: 0, pending_save: None,
        }
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
                    MidiMessage::NoteOn(ch, note, vel) if u8::from(vel) > 0 => {
                        mbsid_sys::note_on(u8::from(ch), u8::from(note), u8::from(vel));
                    }
                    // Note-on vel 0 (running-status note-off) or explicit note-off.
                    MidiMessage::NoteOn(ch, note, _) |
                    MidiMessage::NoteOff(ch, note, _) => {
                        mbsid_sys::note_off(u8::from(ch), u8::from(note));
                    }
                    // Pitch bend: the engine wants the raw 14-bit MIDI value
                    // (msb<<7)|lsb, range 0..16383, center 8192 — exactly what
                    // MbSid.cpp reconstructs from the wire and feeds to
                    // midiReceivePitchBend(). We rebuild it from the raw data
                    // bytes (bytes[1]=LSB, bytes[2]=MSB) to avoid any signed/
                    // centered re-interpretation by midi-types' PitchBend type.
                    MidiMessage::PitchBendChange(ch, _) => {
                        let lsb = (bytes[1] & 0x7F) as u16;
                        let msb = (bytes[2] & 0x7F) as u16;
                        mbsid_sys::pitch_bend(u8::from(ch), (msb << 7) | lsb);
                    }
                    // Control change -> engine CC.
                    MidiMessage::ControlChange(ch, ctrl, val) => {
                        mbsid_sys::cc(u8::from(ch), u8::from(ctrl), u8::from(val));
                    }
                    // Channel aftertouch -> engine aftertouch.
                    MidiMessage::ChannelPressure(ch, val) => {
                        mbsid_sys::aftertouch(u8::from(ch), u8::from(val));
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

        // (a2) Drain the SysEx sideband FIFO (capped per tick). Each byte goes
        // to BOTH consumers: the engine (RAM Writes apply live upstream) and
        // the Rust capture parser (Bank Writes -> pending_save -> main loop).
        let mut got_byte = false;
        for _ in 0..SYSEX_BYTES_PER_TICK {
            let r = sid.sysex_read().read();
            if !r.valid().bit() { break; }
            let b = r.data().bits();
            got_byte = true;
            mbsid_sys::sysex_byte(b);
            if app.sysex_cap.feed(b) {
                let mut buf = [0u8; 512];
                buf.copy_from_slice(app.sysex_cap.data());
                app.pending_save = Some((app.sysex_cap.slot(), buf));
            }
        }
        if got_byte {
            app.sysex_idle_ms = 0;
        } else if app.sysex_cap.in_message() {
            app.sysex_idle_ms = app.sysex_idle_ms.saturating_add(1);
            if app.sysex_idle_ms >= SYSEX_TIMEOUT_MS {
                // Half-received message wedged (cable pulled mid-dump, or an
                // interrupted USB dump with no in-band terminator): abort both
                // parsers so the next dump starts clean.
                mbsid_sys::sysex_timeout();
                app.sysex_cap.reset();
                app.sysex_idle_ms = 0;
            }
        } else {
            app.sysex_idle_ms = 0;
        }

        // (b) Tick the engine; on a register change, diff L and R vs their
        //     shadows and stream only the changed regs to their SIDs.
        if mbsid_sys::tick() {
            let App { diff_l, diff_r, wl, .. } = &mut *app;

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

fn save_status(ok: bool, slot: u8) -> heapless::String<24> {
    let mut s = heapless::String::new();
    let _ = if ok {
        core::fmt::Write::write_fmt(&mut s, format_args!("Saved U{:03}", slot))
    } else {
        core::fmt::Write::write_fmt(&mut s, format_args!("Save FAILED U{:03}", slot))
    };
    s
}

#[entry]
fn main() -> ! {
    let peripherals = pac::Peripherals::take().unwrap();
    let sysclk = pac::clock::sysclk();
    let mut timer = Timer0::new(peripherals.TIMER0, sysclk);

    // Engine bring-up + boot patch (unchanged).
    mbsid_sys::init();
    mbsid_sys::program_change(BOOT_PATCH_INDEX);

    let mut app = App::new();
    app.diff_l.reset();
    app.diff_r.reset();
    let app = Mutex::new(RefCell::new(app));

    // --- Display init ---
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
    display.clear(HI8::BLACK).ok();

    let mut persist = Persist0::new(peripherals.PERSIST_PERIPH);
    persist.set_persistence(MENU_PERSIST);

    let mut encoder = Encoder0::new(peripherals.ENCODER0);

    let spiflash = SPIFlash0::new(
        peripherals.SPIFLASH_CTRL,
        SPIFLASH_BASE,
        SPIFLASH_SZ_BYTES,
    );
    let mut store = UserPatchStore::new(spiflash, USER_BANK_FLASH_BASE);
    // One shared 512B scratch for user-bank load / save / SysEx persist —
    // keeps peak stack usage flat (mainram budget, CLAUDE.md).
    let mut patch_buf = [0u8; 512];
    // Engine/vflags of the last successfully loaded USER patch (the ROM-bank
    // cache in the shim can't answer for user slots).
    let mut user_detail: Option<(u8, u8)> = None;
    let mut status: Option<heapless::String<24>> = None;

    // Total banks = engine ROM banks + the flash User bank (always last).
    let mut state = MenuState::new(mbsid_sys::bank_count() + 1, 0, BOOT_PATCH_INDEX);

    handler!(timer0 = || timer0_handler(&app));

    irq::scope(|s| {
        s.register(Interrupt::TIMER0, timer0);
        timer.enable_tick_isr(TIMER0_ISR_PERIOD_MS, pac::Interrupt::TIMER0);

        let mut dirty = true; // draw once on startup
        loop {
            encoder.update();
            let ticks = encoder.poke_ticks();
            let pressed = encoder.poke_btn();

            let mut need_load = false;
            if ticks != 0 { need_load |= state.on_turn(ticks); dirty = true; }
            if pressed {
                match state.on_press() {
                    PressResult::Toggled => {}
                    PressResult::Cancel => { status = None; }
                    PressResult::Commit(slot) => {
                        // On-device "save as": copy the live patch out under
                        // the ISR guard, then write flash OUTSIDE it (slow).
                        critical_section::with(|_cs| {
                            mbsid_sys::current_patch_raw(&mut patch_buf);
                        });
                        status = Some(save_status(
                            store.save(slot, &patch_buf).is_ok(), slot));
                    }
                }
                dirty = true;
            }

            // SysEx Bank Write captured by the ISR -> persist here.
            let pending = critical_section::with(|cs| {
                app.borrow_ref_mut(cs).pending_save.take()
            });
            if let Some((slot, bytes)) = pending {
                status = Some(save_status(store.save(slot, &bytes).is_ok(), slot));
                dirty = true;
            }

            if need_load {
                if state.is_user_bank() {
                    if store.load(state.program, &mut patch_buf) {
                        critical_section::with(|_cs| {
                            mbsid_sys::load_patch(&patch_buf);
                        });
                        // engine byte 0x10, vflags 0x50 (sid_patch_t layout)
                        user_detail = Some((patch_buf[0x10], patch_buf[0x50]));
                    } else {
                        user_detail = None; // empty slot: engine untouched
                    }
                } else {
                    critical_section::with(|_cs| {
                        mbsid_sys::bank_load(state.bank, state.program);
                    });
                }
            }

            if dirty {
                let mut namebuf = [0u8; 17];
                let name_ok;
                if state.is_user_bank() {
                    let mut n16 = [0u8; 16];
                    name_ok = store.name(state.program, &mut n16);
                    namebuf[..16].copy_from_slice(&n16);
                    namebuf[16] = 0;
                    if !name_ok { namebuf[0] = 0; }
                } else {
                    mbsid_sys::bank_patch_name(state.bank, state.program, &mut namebuf);
                    name_ok = true;
                }
                let name = if name_ok { menu::name_from_cstr(&namebuf) } else { "Empty" };

                // Fetch engine type + voice flags. USER bank uses the last
                // successfully loaded slot's cached detail (the shim's ROM-
                // bank cache can't answer for user slots); ROM banks read
                // directly (read-only, no ISR guard needed). None => show
                // "---" rather than a stale/default Lead/Mono label.
                let detail = if state.is_user_bank() {
                    user_detail.map(|(eng, vfl)| {
                        let e = menu::Engine::from_byte(eng);
                        let vm = if e == menu::Engine::Lead {
                            Some(menu::VoiceMode::from_vflags(vfl))
                        } else { None };
                        (e, vm)
                    })
                } else {
                    mbsid_sys::bank_patch_info(state.bank, state.program).map(|(eng, vfl)| {
                        let e = menu::Engine::from_byte(eng);
                        let vm = if e == menu::Engine::Lead {
                            Some(menu::VoiceMode::from_vflags(vfl))
                        } else { None };
                        (e, vm)
                    })
                };

                // Save-row preview: name of the slot under the cursor.
                let mut savebuf = [0u8; 16];
                let save_name: Option<&str> =
                    if state.save_cursor >= 0
                        && store.name(state.save_cursor as u8, &mut savebuf) {
                        core::str::from_utf8(&savebuf).ok()
                    } else { None };

                menu::draw(&mut display, &state, name, detail,
                           save_name, status.as_deref(),
                           MENU_X, MENU_Y, MENU_HUE).ok();
                dirty = false;
            }
        }
    })
}
