#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

#[cfg(not(test))]
pub use tiliqua_pac as pac;
#[cfg(not(test))]
pub use tiliqua_hal as hal;

#[cfg(not(test))]
hal::impl_tiliqua_soc_pac!();

#[cfg(not(test))]
hal::impl_scope! {
    Scope0: pac::SCOPE_PERIPH,
}

#[cfg(not(test))]
pub mod handlers;

// Host-testable pure modules (no pac dependency)
pub mod bootstrap;
pub mod partition;
pub mod psid;
pub mod sid_scan;

// Embedded-only modules (depend on tiliqua_pac)
#[cfg(not(test))]
pub mod usb_msc;
#[cfg(not(test))]
pub mod fat;
