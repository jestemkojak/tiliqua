#![no_std]
#![no_main]

use log::info;
use riscv_rt::entry;

use tiliqua_pac as pac;
use tiliqua_fw::*;
use tiliqua_lib::*;
use pac::constants::*;

use tiliqua_hal::embedded_graphics::{
    prelude::*,
    mono_font::{MonoTextStyle, ascii::FONT_9X15_BOLD},
    text::Text,
    geometry::Point,
};
use tiliqua_lib::color::HI8;

#[entry]
fn main() -> ! {
    let peripherals = pac::Peripherals::take().unwrap();
    let serial = Serial0::new(peripherals.UART0);

    tiliqua_fw::handlers::logger_init(serial);
    info!("Hello from SID Player!");

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

    let style = MonoTextStyle::new(&FONT_9X15_BOLD, HI8::new(0, 0xB));
    Text::new("SID PLAYER", Point::new(20, 20), style)
        .draw(&mut display)
        .ok();

    loop {}
}
