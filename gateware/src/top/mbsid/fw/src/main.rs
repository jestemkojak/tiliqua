#![no_std]
#![no_main]

//! MBSID-on-Tiliqua dual-SID firmware binary.
//!
//! A 1 kHz Timer0 ISR is the whole engine. Each 1 ms tick it:
//!   1. drains the SIDPeripheral MIDI-in CSR FIFO and feeds events to the
//!      MBSID engine (note on/off, pitch bend, CC);
//!   2. ticks the engine (`mbsid_tick`) at the 1 kHz control rate the host
//!      oracle validated against;
//!   3. diffs the L and R register images vs their 32-byte shadows and
//!      streams only the changed `(data<<5)|addr` words to SIDPeripheral (L)
//!      and SIDPeripheral_R (R) respectively (φ2 = 1 MHz reSID each).
//!
//! The main loop owns everything else: the menu ([`tiliqua_fw::menu`]), USB
//! mass storage, flash persistence, and the display. All real-time work is
//! in the ISR (the VexiiRiscv has no usable mcycle CSR; Timer0 is the only
//! clock — see repo CLAUDE.md).

use core::cell::RefCell;
use critical_section::Mutex;

use amaranth_soc_isr::return_as_is;
use irq::{handler, scoped_interrupts};
use riscv_rt::entry;

use panic_halt as _;

use tiliqua_hal as hal;
use tiliqua_pac as pac;

use tiliqua_fw::bank_import;
use tiliqua_fw::cv::{self, CvSink};
use tiliqua_fw::diag;
use tiliqua_fw::fat::{FileSystem, FsOptions, MscStorage};
use tiliqua_fw::mbsid_sys;
use tiliqua_fw::menu::PressResult;
use tiliqua_fw::menu::{DriveState, UsbInfo};
use tiliqua_fw::patch_store::{UserPatchStore, USER_BANK_FLASH_BASE};
use tiliqua_fw::regdiff::{RegDiff, WriteList};
use tiliqua_fw::status::{self, Status};
use tiliqua_fw::sysex_capture::SysexCapture;
use tiliqua_fw::usb_patch;
use tiliqua_fw::{params, settings_store, uptime};

use midi_convert::parse::MidiTryParseSlice;
use midi_types::MidiMessage;

use pac::constants::*;
use tiliqua_fw::menu::{self, MenuState, TurnResult};
use tiliqua_hal::encoder::Encoder;
use tiliqua_hal::persist::Persist;
use tiliqua_hal::pmod::EurorackPmod;
use tiliqua_lib::{bootinfo, calibration, palette};

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
const BOOT_PATCH_INDEX: u8 = 0;

const MENU_X: i32 = 60;
const MENU_Y: i32 = 80;
const MENU_HUE: u8 = 10;
// Persist decay setting. With persist_freeze_rows=320 in top.py, the menu band
// is frozen from decay entirely; this value only governs rows >= 320, which
// hold nothing (mbsid has no scope) and stay in persist's all-zero fastpath.
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
        unsafe {
            TIMER0();
        }
        timer.clear_pending();
    }
}

/// Engine-adjacent ISR state: per-SID register-diff shadows (L/R) and the
/// shared scratch write list, shared with the Timer0 ISR via `Mutex<RefCell<App>>`.
struct App {
    diff_l: RegDiff,
    diff_r: RegDiff,
    wl: WriteList,
    sysex_cap: SysexCapture,
    sysex_idle_ms: u16,
    /// Complete Bank Write captured by the ISR; persisted by the main loop
    /// (flash I/O must never run in the 1 ms ISR).
    pending_save: Option<(u8, [u8; 512])>,
    /// CV-input modulation routing state (M5 §6b).
    cv: cv::CvState,
    /// Calibrated Eurorack CV/gate input jacks, sampled once per ISR tick.
    pmod: EurorackPmod0,
}

impl App {
    fn new(pmod: EurorackPmod0) -> Self {
        Self {
            diff_l: RegDiff::new(),
            diff_r: RegDiff::new(),
            wl: WriteList::new(),
            sysex_cap: SysexCapture::new(),
            sysex_idle_ms: 0,
            pending_save: None,
            cv: cv::CvState::new(),
            pmod,
        }
    }
}

/// Patch persistence: the flash-backed user bank, the single shared 512 B
/// scratch buffer every save/load path reuses (keeping peak stack flat —
/// mainram budget, CLAUDE.md), and the engine/vflags of the last
/// successfully loaded USER patch (the ROM-bank cache in the shim can't
/// answer for user slots).
struct Storage {
    store: UserPatchStore<SPIFlash0>,
    patch_buf: [u8; 512],
    user_detail: Option<(u8, u8)>,
}

/// USB mass storage: the CSR driver plus the cached directory listing.
/// `listed` guards the (relatively expensive) directory scan; `keepalive_at`
/// paces the idle LBA-0 probe.
struct Usb {
    msc: tiliqua_fw::usb_msc::UsbMsc,
    files: usb_patch::FileList,
    listed: bool,
    keepalive_at: u32,
}

/// Display, its diff-painter, and the encoder — everything the UI touches
/// each frame. `Persist0` is deliberately not here: it is configured once at
/// init and never read again.
struct Ui {
    display: DMAFramebuffer0,
    painter: menu::Painter,
    encoder: Encoder0,
}

/// Derive USB state from live hardware, every iteration (M5 lesson).
/// Returns `(drive_present, dirty)`.
///
/// Deriving `Card::Usb`'s validity only from menu navigation events would
/// let a stale file list survive an unplug until the user next turned the
/// encoder — same shape as the M5 `lead_loaded` bug.
fn sync_usb_state(usb: &mut Usb, state: &mut MenuState) -> (bool, bool) {
    let mut dirty = false;

    // Leaving Storage mode (or losing the drive) collapses the Usb card and
    // invalidates the cached file list. `state.card` is reassigned directly
    // here (not via `Card::step`) because this is an unconditional collapse,
    // not a navigation event — but it lands on the same value `step` would
    // clamp to on the very next Card turn once `usb_storage` is false (see
    // `Card::step`'s doc comment), so there is no window where
    // `state.card == Usb` survives with `usb_storage == false`.
    if !state.usb_storage && state.card == menu::Card::Usb {
        state.card = menu::Card::Main;
        state.focus = menu::ROW_CARD;
        dirty = true;
    }

    // `connected()`, NOT `ready()`: `ready()` is `~busy`, so it is
    // legitimately 0 for the duration of ANY in-flight command, including
    // the idle keepalive's own read below (read_block() returns as soon as
    // the 512 data bytes are drained, before the engine's CSW/READY
    // housekeeping finishes — so a real, if brief, ready=0 window follows
    // every read, keepalive included). Using `ready()` here raced the
    // keepalive against its own presence check and periodically
    // collapsed+rebuilt the Usb file list (visible flicker) even with
    // nothing unplugged. `connected()` is the persistent enumeration flag.
    let drive_present = state.usb_storage && usb.msc.connected() && usb.msc.block_size() == 512;

    // Idle keepalive: the MSC engine's watchdog (vendor msc.py) is
    // handshake-fed since round seven — any ACK/NAK/NYET holds it cleared —
    // but an IDLE bus produces no handshakes at all (SOFs don't touch the
    // SIE response), so a quiet READY drive would still be reset every 10 s
    // without probe traffic. The keepalive is ALSO what turns an unplug into
    // evidence: a yanked drive answers the probe with silence (TIMEOUT), the
    // watchdog runs out, `ready` drops. Do not remove it in either direction.
    if drive_present {
        const MSC_KEEPALIVE_MS: u32 = 2000;
        let now = uptime::now_ms();
        if now.wrapping_sub(usb.keepalive_at) >= MSC_KEEPALIVE_MS {
            usb.keepalive_at = now;
            let mut scratch = [0u8; 512];
            let _ = usb.msc.read_block(0, &mut scratch);
        }
    }

    if !drive_present && usb.listed {
        usb.listed = false; // drive unplugged / mode left
        usb.files.clear();
        state.usb_file = -1;
        state.usb_file_count = 0;
        if state.card == menu::Card::Usb {
            dirty = true;
        }
    }
    if drive_present && !usb.listed && state.card == menu::Card::Usb {
        usb.files.clear();
        let n = with_fat(&usb.msc, |fs| {
            usb_patch::list_patch_files(fs, &mut usb.files)
        })
        .unwrap_or(0);
        state.usb_file_count = n as u8;
        state.usb_file = if n > 0 { 0 } else { -1 };
        usb.listed = true;
        dirty = true;
    }

    (drive_present, dirty)
}

/// Dispatch an encoder turn. Returns `true` if a patch load is needed.
fn handle_turn(
    r: TurnResult,
    state: &mut MenuState,
    app: &Mutex<RefCell<App>>,
    settings_dirty_at: &mut Option<u32>,
) -> bool {
    match r {
        TurnResult::Load => return true,
        TurnResult::Param { ix, value } => {
            let d = &params::LEAD_PARAMS[ix as usize];
            let ops = params::write_ops(d, value, |a| mbsid_sys::patch_byte(a));
            critical_section::with(|_cs| {
                for (a, v) in ops.iter() {
                    mbsid_sys::sysex_param(*a, *v);
                }
            });
            state.edited = true;
        }
        TurnResult::SettingsChanged => {
            critical_section::with(|cs| {
                let mut a = app.borrow_ref_mut(cs);
                let App { cv, .. } = &mut *a;
                cv.set_targets(state.cv_targets, &mut EngineSink);
            });
            *settings_dirty_at = Some(uptime::now_ms());
        }
        TurnResult::None => {}
    }
    false
}

/// Dispatch an encoder press. Returns the new status text, if any.
/// `PressResult::Cancel` clears the status and is handled by the caller,
/// which owns the `Status`.
fn handle_press(
    r: PressResult,
    state: &mut MenuState,
    storage: &mut Storage,
    usb: &mut Usb,
    ser: &mut impl core::fmt::Write,
) -> Option<heapless::String<24>> {
    match r {
        PressResult::Toggled | PressResult::Cancel => None,
        PressResult::Commit(slot) => {
            // On-device "save as": copy the live patch out under the ISR
            // guard, then write flash OUTSIDE it (slow).
            critical_section::with(|_cs| {
                mbsid_sys::current_patch_raw(&mut storage.patch_buf);
            });
            let ok = storage.store.save(slot, &storage.patch_buf).is_ok();
            if ok {
                state.edited = false;
            }
            Some(status::saved(ok, slot))
        }
        PressResult::UsbLoad(ix) => Some(usb_load(
            &usb.msc,
            ix as usize,
            None,
            &mut storage.store,
            &mut storage.patch_buf,
            state,
            &mut storage.user_detail,
        )),
        PressResult::UsbLoadToSlot { file, slot } => Some(usb_load(
            &usb.msc,
            file as usize,
            Some(slot),
            &mut storage.store,
            &mut storage.patch_buf,
            state,
            &mut storage.user_detail,
        )),
        PressResult::UsbImportBank => {
            // Whole-bank replace from /MBSID/BANK.SYX (spec §4). Runs
            // synchronously; the frozen menu is the busy signal, same
            // contract as export. Import writes internal SPI flash only —
            // no drive writes, so no usb.msc.flush() needed.
            let outcome = with_fat(&usb.msc, |fs| {
                bank_import::import_bank(fs, &mut storage.store)
            });
            Some(status::imported(outcome))
        }
        PressResult::UsbExport { source } => {
            // Re-enabled 2026-07-14 after the drive-corruption incident was
            // root-caused and fixed (payload-less WRITE(10) from the CSR
            // TX-FIFO flush-on-strobe; now strobe-then-fill + deferred
            // engine start, see M6_USB_STORAGE.md's incident writeup).
            // Hardware re-tested 2026-07-16: multiple exports produced
            // byte-correct .SYX files (header/slot/checksum all verified
            // against the source patch) with no drive damage — permanently
            // enabled.
            let (slot, got) = match source {
                menu::ExportSource::Edit => {
                    critical_section::with(|_cs| {
                        mbsid_sys::current_patch_raw(&mut storage.patch_buf);
                    });
                    (0u8, true)
                }
                menu::ExportSource::Slot(n) => (n, storage.store.load(n, &mut storage.patch_buf)),
            };
            let fname = menu::export_name(source);
            let snap0 = diag::export_begin(ser, &usb.msc, &fname, got);
            let mounted = core::cell::Cell::new(false);
            let ok = got
                && with_fat(&usb.msc, |fs| {
                    mounted.set(true);
                    usb_patch::export_patch(fs, &fname, &storage.patch_buf, slot)
                })
                .unwrap_or(false);
            // Commit the drive's volatile cache before reporting success —
            // the verify read may have been served from cache (round eight
            // durability fix).
            let ok = ok && usb.msc.flush().is_ok();
            diag::export_result(ser, &usb.msc, ok, mounted.get(), &snap0);
            usb.listed = false; // new file: refresh the list
            Some(status::exported(ok, &fname))
        }
    }
}

/// Load the patch the menu currently points at, from either an engine ROM
/// bank or the flash user bank.
///
/// `state.lead_loaded` is deliberately NOT touched here — it is resynced
/// unconditionally at the top of the next loop iteration, which is the only
/// place that also covers async MIDI Program Change.
fn do_load(state: &mut MenuState, storage: &mut Storage) {
    if state.is_user_bank() {
        if storage.store.load(state.program, &mut storage.patch_buf) {
            critical_section::with(|_cs| {
                mbsid_sys::load_patch(&storage.patch_buf);
            });
            storage.user_detail = Some(params::patch_detail_bytes(&storage.patch_buf));
            state.refresh_params(|a| mbsid_sys::patch_byte(a));
            state.edited = false;
        } else {
            storage.user_detail = None; // empty slot: engine untouched
        }
    } else {
        critical_section::with(|_cs| {
            mbsid_sys::bank_load(state.bank, state.program);
        });
        state.refresh_params(|a| mbsid_sys::patch_byte(a));
        state.edited = false;
    }
}

/// Repaint the menu. Also applies the menu's MIDI-source and USB-mode
/// selections to the gateware: both are unconditional/idempotent CSR writes,
/// cheap enough to run every redraw, no change-tracking needed (mirrors
/// top/sid's identical call).
fn render(
    ui: &mut Ui,
    sid: &pac::SID_PERIPH,
    state: &MenuState,
    storage: &mut Storage,
    usb: &Usb,
    status: &Status,
    drive_present: bool,
) {
    sid.usb_midi_host().write(|w| {
        w.host()
            .bit(state.midi_src == menu::MidiSource::Usb && !state.usb_storage)
    });
    usb.msc.set_mode(state.usb_storage);

    let mut namebuf = [0u8; 17];
    let name_ok;
    if state.is_user_bank() {
        let mut n16 = [0u8; 16];
        name_ok = storage.store.name(state.program, &mut n16);
        namebuf[..16].copy_from_slice(&n16);
        namebuf[16] = 0;
        if !name_ok {
            namebuf[0] = 0;
        }
    } else {
        mbsid_sys::bank_patch_name(state.bank, state.program, &mut namebuf);
        name_ok = true;
    }
    let name = if name_ok {
        menu::name_from_cstr(&namebuf)
    } else {
        "Empty"
    };

    // Fetch engine type + voice flags. USER bank uses the last successfully
    // loaded slot's cached detail (the shim's ROM-bank cache can't answer for
    // user slots); ROM banks read directly (read-only, no ISR guard needed).
    // None => show "---" rather than a stale/default Lead/Mono label.
    let detail = if state.is_user_bank() {
        storage.user_detail
    } else {
        mbsid_sys::bank_patch_info(state.bank, state.program)
    }
    .map(|(eng, vfl)| menu::patch_detail(eng, vfl));

    // Save-row preview: name of the slot under the cursor.
    let mut savebuf = [0u8; 16];
    let save_name: Option<&str> = if state.save_cursor >= 0
        && storage.store.name(state.save_cursor as u8, &mut savebuf)
    {
        core::str::from_utf8(&savebuf).ok()
    } else {
        None
    };

    // Usb card detail: drive/file/slot names, built only while the card is
    // focused (borrows usb.files/slotbuf).
    let mut slotbuf = [0u8; 16];
    let usb_info = if state.card == menu::Card::Usb {
        let slot_name: Option<&str> = if state.usb_slot >= 0
            && storage.store.name(state.usb_slot as u8, &mut slotbuf)
        {
            core::str::from_utf8(&slotbuf).ok()
        } else {
            None
        };
        Some(UsbInfo {
            drive: if drive_present {
                DriveState::Ready
            } else {
                DriveState::NoDrive
            },
            file_name: if state.usb_file >= 0 {
                usb.files.get(state.usb_file as usize).map(|n| n.as_str())
            } else {
                None
            },
            file_count: state.usb_file_count,
            slot_name,
        })
    } else {
        None
    };

    // Diff-paint: blitter-only, erases stale glyphs by re-blitting old text
    // at intensity 0 — no rectangle fill, no visible wipe (see menu::Painter).
    let frame = menu::build_frame(
        state,
        name,
        detail,
        save_name,
        status.as_deref(),
        state.lead_loaded,
        usb_info.as_ref(),
        MENU_X,
        MENU_Y,
    );
    ui.painter.paint(&mut ui.display, frame, MENU_HUE).ok();
}

/// Persist menu settings ~2 s after the last change (flash wear; M5 §6d).
/// Skipped entirely if nothing actually changed.
fn persist_settings(
    state: &MenuState,
    storage: &mut Storage,
    window_start: u32,
    last_saved: &mut settings_store::Settings,
    dirty_at: &mut Option<u32>,
) {
    let Some(t0) = *dirty_at else { return };
    if !uptime::deadline_expired(t0, uptime::now_ms(), 2000) {
        return;
    }
    let s = settings_store::Settings {
        midi_src: (state.midi_src == menu::MidiSource::Usb) as u8,
        cv_targets: state.cv_targets.map(|t| t.to_u8()),
        usb_mode: state.usb_storage as u8,
    };
    if s != *last_saved {
        let _ = settings_store::save(storage.store.flash_mut(), window_start, &s);
        *last_saved = s;
    }
    *dirty_at = None;
}

/// CvSink implementation over the engine FFI. Only used inside
/// critical_section (ISR body, or main-loop blocks under `cs`).
struct EngineSink;
impl CvSink for EngineSink {
    fn knob(&mut self, knob: u8, value: u8) {
        mbsid_sys::knob_set(knob, value);
    }
    fn par(&mut self, par: u8, value16: u16) {
        mbsid_sys::par_set(par, value16);
    }
    fn note_on(&mut self, note: u8) {
        mbsid_sys::note_on(0, note, 100);
    } // MIDI ch 1
    fn note_off(&mut self, note: u8) {
        mbsid_sys::note_off(0, note);
    }
}

/// Drain a write list to a SID peripheral, respecting FIFO backpressure
/// (poll txn_status.writable). `(data<<5)|addr` encoding == `top/sid`.
/// Closures abstract over the two distinct PAC peripheral types.
fn drain_writelist(wl: &WriteList, writable: impl Fn() -> bool, write_word: impl Fn(u16)) {
    for (reg, val) in wl.iter() {
        while !writable() {}
        write_word(((*val as u16) << 5) | (*reg as u16));
    }
}

/// 1 kHz control ISR: MIDI in -> engine -> tick -> diff -> SID writes.
fn timer0_handler(app: &Mutex<RefCell<App>>) {
    let peripherals = unsafe { pac::Peripherals::steal() };
    let sid = peripherals.SID_PERIPH;
    let sid_r = peripherals.SID_PERIPH_R;

    critical_section::with(|cs| {
        let mut app = app.borrow_ref_mut(cs);

        // (a) Drain the MIDI-in CSR FIFO (read until 0, as top/sid does) and
        //     dispatch each parsed message into the engine.
        loop {
            let word = sid.midi_read().read().bits();
            if word == 0 {
                break;
            }
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
                    MidiMessage::NoteOn(ch, note, _) | MidiMessage::NoteOff(ch, note, _) => {
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
            if !r.valid().bit() {
                break;
            }
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

        // (a3) CV modulation: sample the calibrated inputs and route per the
        // menu's target assignments (M5 §6b). Integer-only; engine calls are
        // the same knob/par paths MIDI CC takes.
        uptime::tick_1ms();
        let x = app.pmod.sample_i();
        let App { cv, .. } = &mut *app;
        cv.tick(x, &mut EngineSink);

        // (b) Tick the engine; on a register change, diff L and R vs their
        //     shadows and stream only the changed regs to their SIDs.
        if mbsid_sys::tick() {
            let App {
                diff_l, diff_r, wl, ..
            } = &mut *app;

            diff_l.update(mbsid_sys::regs_l(), wl);
            drain_writelist(
                wl,
                || sid.txn_status().read().writable().bit(),
                |w| {
                    sid.transaction_data()
                        .write(|r| unsafe { r.transaction_data().bits(w) });
                },
            );
            wl.clear();

            diff_r.update(mbsid_sys::regs_r(), wl);
            drain_writelist(
                wl,
                || sid_r.txn_status().read().writable().bit(),
                |w| {
                    sid_r
                        .transaction_data()
                        .write(|r| unsafe { r.transaction_data().bits(w) });
                },
            );
            wl.clear();
        }
    });
}

/// Mount the drive's first FAT volume and run `f` on it. Every USB menu
/// action re-mounts (sid_player_sw idiom): no FileSystem lifetime to hold
/// across drive unplugs, and patch files are tiny so the cost is a few
/// 512-byte reads.
fn with_fat<R>(
    msc: &tiliqua_fw::usb_msc::UsbMsc,
    f: impl FnOnce(&FileSystem<MscStorage<&tiliqua_fw::usb_msc::UsbMsc>>) -> R,
) -> Option<R> {
    let storage = MscStorage::new(msc);
    match FileSystem::new(storage, FsOptions::new()) {
        Ok(fs) => Some(f(&fs)),
        Err(_) => None,
    }
}

/// Load USB patch file `ix` into the engine (audition); optionally also
/// persist it to user-bank `slot`. Same engine entry as the SysEx path
/// (`load_patch`), so behavior is provably identical to a MIDI upload of
/// the same bytes (spec §6e).
fn usb_load<F: tiliqua_hal::nor_flash::NorFlash + tiliqua_hal::nor_flash::ReadNorFlash>(
    msc: &tiliqua_fw::usb_msc::UsbMsc,
    ix: usize,
    slot: Option<u8>,
    store: &mut UserPatchStore<F>,
    patch_buf: &mut [u8; 512],
    state: &mut MenuState,
    user_detail: &mut Option<(u8, u8)>,
) -> heapless::String<24> {
    let ok = with_fat(msc, |fs| usb_patch::load_patch_by_index(fs, ix, patch_buf)).unwrap_or(false);
    if !ok {
        return status::usb_load_failed();
    }
    critical_section::with(|_cs| {
        mbsid_sys::load_patch(patch_buf);
    });
    *user_detail = Some(params::patch_detail_bytes(patch_buf));
    state.refresh_params(|a| mbsid_sys::patch_byte(a));
    state.edited = false;
    match slot {
        Some(n) => status::usb_loaded(Some(n), store.save(n, patch_buf).is_ok()),
        None => status::usb_loaded(None, true),
    }
}

#[entry]
fn main() -> ! {
    let peripherals = pac::Peripherals::take().unwrap();
    let sysclk = pac::clock::sysclk();
    let mut timer = Timer0::new(peripherals.TIMER0, sysclk);

    // Bring-up diagnostics over UART0. mbsid has no logger/UI wiring (unlike
    // sid_player_sw's `handlers::logger_init`), so this talks to Serial0
    // directly. Inert unless `usb-diag`/`stack-probe` is on.
    let mut diag_serial = Serial0::new(peripherals.UART0);

    // Stack high-water measurement (M6_USB_STORAGE.md §7a). No-op unless the
    // `stack-probe` feature is on.
    diag::stack_paint(&mut diag_serial);

    // Engine bring-up + boot patch (unchanged).
    mbsid_sys::init();
    mbsid_sys::program_change(BOOT_PATCH_INDEX);

    let mut i2cdev1 = I2c1::new(peripherals.I2C1);
    let mut pmod = EurorackPmod0::new(peripherals.PMOD0_PERIPH);
    calibration::CalibrationConstants::load_or_default(&mut i2cdev1, &mut pmod);

    let mut app = App::new(pmod);
    app.diff_l.reset();
    app.diff_r.reset();
    let app = Mutex::new(RefCell::new(app));

    // --- Display init ---
    let bootinfo = unsafe { bootinfo::BootInfo::from_addr(BOOTINFO_BASE) }.unwrap();
    let modeline = bootinfo
        .modeline
        .maybe_override_fixed(FIXED_MODELINE, CLOCK_DVI_HZ);
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

    let mut ui = Ui {
        display,
        painter: menu::Painter::new(),
        encoder: Encoder0::new(peripherals.ENCODER0),
    };

    let spiflash = SPIFlash0::new(peripherals.SPIFLASH_CTRL, SPIFLASH_BASE, SPIFLASH_SZ_BYTES);
    let mut storage = Storage {
        store: UserPatchStore::new(spiflash, USER_BANK_FLASH_BASE),
        patch_buf: [0u8; 512],
        user_detail: None,
    };
    let mut status = Status::new();

    // Total banks = engine ROM banks + the flash User bank (always last).
    let mut state = MenuState::new(mbsid_sys::bank_count() + 1, 0, BOOT_PATCH_INDEX);
    state.lead_loaded = mbsid_sys::current_engine() == 0;
    state.refresh_params(|a| mbsid_sys::patch_byte(a));

    // Load persisted settings (MIDI source, CV target assignments) from the
    // option-storage flash window, if the manifest provides one. Any
    // validation failure (blank flash, wrong magic/version, bad checksum)
    // decodes to defaults (TRS, all CV Off) — see settings_store.rs.
    let opt_window = bootinfo.manifest.get_option_storage_window();
    let settings = match opt_window {
        Some(ref w) => settings_store::load(storage.store.flash_mut(), w.start),
        None => settings_store::Settings::default(),
    };
    state.midi_src = if settings.midi_src == 1 {
        menu::MidiSource::Usb
    } else {
        menu::MidiSource::Trs
    };
    state.cv_targets = settings.cv_targets.map(cv::CvTarget::from_u8);
    state.usb_storage = settings.usb_mode == 1;
    let mut settings_dirty_at: Option<u32> = None;
    let mut last_saved = settings;

    // Seed the ISR's CV routing once before entering the tick loop (no note
    // can be held yet, EngineSink is inert until the engine is boot-loaded).
    critical_section::with(|cs| {
        let mut a = app.borrow_ref_mut(cs);
        let App { cv, .. } = &mut *a;
        cv.set_targets(state.cv_targets, &mut EngineSink);
    });

    // Drives the gateware USB/TRS MIDI source mux (top/sid's usb_midi_host
    // CSR, inherited unchanged — see menu.rs's MidiSrc row). Bound once here;
    // the ISR's own SID_PERIPH access uses an independent `Peripherals::steal()`.
    let sid = peripherals.SID_PERIPH;

    // M6a: USB mass-storage. All access is main-loop-only; a slow drive
    // stalls UI redraw, never audio (the ISR keeps ticking the engine).
    let mut usb = Usb {
        msc: tiliqua_fw::usb_msc::UsbMsc::new(peripherals.USB_MSC),
        files: usb_patch::FileList::new(),
        listed: false,
        keepalive_at: 0,
    };

    handler!(timer0 = || timer0_handler(&app));

    // Bring-up diagnostic state. Inert unless the corresponding cargo
    // feature is on (see diag.rs).
    let mut stack_probe_max: usize = 0;
    let mut stack_probe_ctr: u32 = 0;
    let mut usb_diag_last = diag::UsbStatusSnap::default();

    irq::scope(|s| {
        s.register(Interrupt::TIMER0, timer0);
        timer.enable_tick_isr(TIMER0_ISR_PERIOD_MS, pac::Interrupt::TIMER0);

        let mut dirty = true; // draw once on startup
        loop {
            diag::stack_scan(&mut diag_serial, &mut stack_probe_max, &mut stack_probe_ctr);

            ui.encoder.update();
            let ticks = ui.encoder.poke_ticks();
            let pressed = ui.encoder.poke_btn();

            // Resync unconditionally every iteration: an inbound MIDI Program
            // Change (timer0_handler, async w.r.t. menu navigation) can swap
            // the engine's loaded patch without going through `need_load`
            // below. Without this, `state.lead_loaded` can go stale between
            // two `on_turn` calls and PatchEdit's row_count()/on_turn would
            // treat a non-Lead patch as Lead, re-opening the byte-offset
            // corruption the prior fix closed.
            state.lead_loaded = mbsid_sys::current_engine() == 0;

            if status.expire(uptime::now_ms()) {
                dirty = true;
            }

            let (drive_present, usb_dirty) = sync_usb_state(&mut usb, &mut state);
            dirty |= usb_dirty;
            diag::usb_status(&mut diag_serial, &usb.msc, &mut usb_diag_last);

            let mut need_load = false;
            if ticks != 0 {
                let turn = state.on_turn(ticks);
                need_load = handle_turn(turn, &mut state, &app, &mut settings_dirty_at);
                dirty = true;
            }
            if pressed {
                match state.on_press() {
                    PressResult::Cancel => status.clear(),
                    r => {
                        if let Some(text) =
                            handle_press(r, &mut state, &mut storage, &mut usb, &mut diag_serial)
                        {
                            status.set(text, uptime::now_ms());
                        }
                    }
                }
                dirty = true;
            }

            // SysEx Bank Write captured by the ISR -> persist here.
            let pending = critical_section::with(|cs| app.borrow_ref_mut(cs).pending_save.take());
            if let Some((slot, bytes)) = pending {
                status.set(
                    status::saved(storage.store.save(slot, &bytes).is_ok(), slot),
                    uptime::now_ms(),
                );
                dirty = true;
            }

            if need_load {
                do_load(&mut state, &mut storage);
            }

            if let Some(ref w) = opt_window {
                persist_settings(
                    &state,
                    &mut storage,
                    w.start,
                    &mut last_saved,
                    &mut settings_dirty_at,
                );
            }

            if dirty {
                render(
                    &mut ui,
                    &sid,
                    &state,
                    &mut storage,
                    &usb,
                    &status,
                    drive_present,
                );
                dirty = false;
            }
        }
    })
}
