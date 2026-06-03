#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

pub use tiliqua_pac as pac;
pub use tiliqua_hal as hal;

#[cfg(not(test))]
hal::impl_tiliqua_soc_pac!();

#[cfg(not(test))]
pub mod handlers;

pub mod bootstrap;
pub mod psid;
