#![cfg_attr(not(test), no_std)]

pub mod cv;
pub mod frame;
pub mod regdiff;
pub mod mbsid_sys;
pub mod menu;
pub mod sysex_capture;
pub mod patch_store;
pub mod params;
pub mod settings_store;
pub mod partition;
pub mod fat;
// pub mod usb_patch;          // Task 6 (add the file in that task; keep this line commented until then)
#[cfg(not(test))]
pub mod usb_msc;            // pac-dependent: embedded only
