pub const UI_NAME: &str = "SID-PLAYER";
pub const UI_TAG: &str = "935b7d9";
pub const HW_REV_MAJOR: u32 = 5;
pub const USE_EXTERNAL_PLL: bool = true;
pub const CLOCK_SYNC_HZ: u32 = 60000000;
pub const CLOCK_AUDIO_HZ: u32 = 12288000;
pub const CLOCK_DVI_HZ: u32 = 74250000;
pub const FIXED_MODELINE: Option<(u16, u16)> = None;
pub const PSRAM_BASE: usize = 0x20000000;
pub const PSRAM_SZ_BYTES: usize = 0x1000000;
pub const PSRAM_SZ_WORDS: usize = PSRAM_SZ_BYTES / 4;
pub const SPIFLASH_BASE: usize = 0x10000000;
pub const SPIFLASH_SZ_BYTES: usize = 0x1000000;
pub const PSRAM_FB_BASE: usize = 0x20000000;
pub const N_BITSTREAMS: usize = 8;
pub const BOOTINFO_BASE: usize = 0x20fff000;
pub const TOUCH_SENSOR_ORDER: [u8; 8] = [5, 7, 8, 9, 10, 11, 12, 13];
pub const PMOD_DEFAULT_CAL: [f32; 4] = [-1.248, -0.03, 0.9, 0.0];
pub const BLIT_MEM_BASE: usize = 0xc0000000;
pub const AUDIO_FS: u32 = 48000;
// Extra constants specified by an SoC subclass:
pub const MODULE_DOCSTRING: &str =
    r###"SID player bitstream: arlet 6502 runs PSID init/play, writes the SID core."###;
