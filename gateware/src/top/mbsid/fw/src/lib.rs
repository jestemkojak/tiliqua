//! MBSID-on-Tiliqua firmware library.
//!
//! The host-testable half of the `top/mbsid` firmware. `main.rs` is the
//! riscv32 binary and is excluded from `cargo test --lib`; everything that
//! can be tested on the host lives here.
//!
//! Layout:
//! - Engine bridge: [`mbsid_sys`] (FFI to the vendored C++ MBSID engine,
//!   cfg-stubbed on host), [`regdiff`] (SID register-image diffing).
//! - Menu and rendering: [`menu`] (pure state machine + frame builder),
//!   [`frame`] (positioned-string frames), [`params`] (Lead patch parameter
//!   table and `sid_patch_t` layout), [`status`] (the status line).
//! - Patch persistence: [`patch_store`] (128-slot flash user bank),
//!   [`settings_store`] (menu settings), [`sysex_capture`] (MIDI SysEx Bank
//!   Write parser), [`bank_import`] (whole-bank replace from BANK.SYX).
//! - USB mass storage: `usb_msc` (CSR driver), [`fat`]/[`partition`]
//!   (block IO and FAT plumbing), `usb_patch` (patch file load/export).
//! - Misc: [`cv`] (CV-input modulation routing), [`uptime`] (Timer0
//!   wall-clock), `diag` (feature-gated bring-up tracing).
//!
//! `usb_msc` and `diag` are `#[cfg(not(test))]` — they depend on the PAC and
//! are embedded-only.

#![cfg_attr(not(test), no_std)]

pub mod bank_import;
pub mod cv;
// pac-dependent (reads usb_msc): embedded only.
#[cfg(not(test))]
pub mod diag;
pub mod fat;
pub mod frame;
pub mod mbsid_sys;
pub mod menu;
pub mod params;
pub mod partition;
pub mod patch_store;
pub mod regdiff;
pub mod settings_store;
pub mod status;
pub mod sysex_capture;
pub mod uptime;
#[cfg(not(test))]
pub mod usb_msc;
pub mod usb_patch; // pac-dependent: embedded only
