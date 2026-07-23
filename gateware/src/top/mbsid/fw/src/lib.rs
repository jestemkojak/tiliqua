#![cfg_attr(not(test), no_std)]

pub mod bank_import;
pub mod cv;
pub mod fat;
pub mod frame;
pub mod mbsid_sys;
pub mod menu;
pub mod params;
pub mod partition;
pub mod patch_store;
pub mod regdiff;
pub mod settings_store;
pub mod sysex_capture;
pub mod uptime;
#[cfg(not(test))]
pub mod usb_msc;
pub mod usb_patch; // pac-dependent: embedded only
