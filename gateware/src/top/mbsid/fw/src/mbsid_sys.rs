// mbsid_sys.rs — FFI declarations + safe wrappers for mbsid_shim ABI.
// The extern "C" link block is gated to riscv32 only; host builds use stubs.

#[cfg(target_arch = "riscv32")]
extern "C" {
    fn mbsid_init();
    fn mbsid_load_patch(buf512: *const u8) -> i32;
    fn mbsid_note_on(chn: u8, note: u8, vel: u8);
    fn mbsid_note_off(chn: u8, note: u8);
    fn mbsid_pitch_bend(chn: u8, bend14: u16);
    fn mbsid_cc(chn: u8, cc: u8, val: u8);
    fn mbsid_aftertouch(chn: u8, val: u8);
    fn mbsid_tick(speed_factor: u8) -> i32;
    fn mbsid_regs_l() -> *const u8;
    fn mbsid_regs_r() -> *const u8;
    fn mbsid_program_change(patch: u8);
    fn mbsid_bank_count() -> u8;
    fn mbsid_bank_load(bank: u8, patch: u8) -> i32;
    fn mbsid_bank_patch_name_get(bank: u8, patch: u8, buf: *mut core::ffi::c_char);
    fn mbsid_bank_patch_info(bank: u8, patch: u8,
                              engine_out: *mut u8, vflags_out: *mut u8) -> i32;
}

// --- safe wrappers (riscv32 target) ---

#[cfg(target_arch = "riscv32")]
pub fn init() {
    unsafe { mbsid_init() }
}

#[cfg(target_arch = "riscv32")]
pub fn tick() -> bool {
    // speed_factor=0: the arg is accepted by the shim ABI but ignored — the
    // engine uses its internal updateSpeedFactor=2 set by MbSidEnvironment ctor.
    // shim_driver.cpp passes 2 for documentation clarity; both are equivalent.
    unsafe { mbsid_tick(0) != 0 }
}

#[cfg(target_arch = "riscv32")]
pub fn regs_l() -> &'static [u8; 32] {
    unsafe { &*(mbsid_regs_l() as *const [u8; 32]) }
}

#[cfg(target_arch = "riscv32")]
pub fn regs_r() -> &'static [u8; 32] {
    unsafe { &*(mbsid_regs_r() as *const [u8; 32]) }
}

#[cfg(target_arch = "riscv32")]
pub fn load_patch(buf: &[u8; 512]) -> bool {
    unsafe { mbsid_load_patch(buf.as_ptr()) == 0 }
}

#[cfg(target_arch = "riscv32")]
pub fn program_change(patch: u8) {
    unsafe { mbsid_program_change(patch) }
}

#[cfg(target_arch = "riscv32")]
pub fn note_on(chn: u8, note: u8, vel: u8) {
    unsafe { mbsid_note_on(chn, note, vel) }
}

#[cfg(target_arch = "riscv32")]
pub fn note_off(chn: u8, note: u8) {
    unsafe { mbsid_note_off(chn, note) }
}

#[cfg(target_arch = "riscv32")]
pub fn pitch_bend(chn: u8, bend14: u16) {
    unsafe { mbsid_pitch_bend(chn, bend14) }
}

#[cfg(target_arch = "riscv32")]
pub fn cc(chn: u8, cc: u8, val: u8) {
    unsafe { mbsid_cc(chn, cc, val) }
}

#[cfg(target_arch = "riscv32")]
pub fn aftertouch(chn: u8, val: u8) {
    unsafe { mbsid_aftertouch(chn, val) }
}

#[cfg(target_arch = "riscv32")]
pub fn bank_count() -> u8 {
    unsafe { mbsid_bank_count() }
}

#[cfg(target_arch = "riscv32")]
pub fn bank_load(bank: u8, patch: u8) -> bool {
    unsafe { mbsid_bank_load(bank, patch) == 0 }
}

#[cfg(target_arch = "riscv32")]
pub fn bank_patch_name(bank: u8, patch: u8, buf: &mut [u8; 17]) {
    unsafe { mbsid_bank_patch_name_get(bank, patch, buf.as_mut_ptr() as *mut core::ffi::c_char) }
}

#[cfg(target_arch = "riscv32")]
pub fn bank_patch_info(bank: u8, patch: u8) -> Option<(u8, u8)> {
    let mut eng = 0u8;
    let mut vfl = 0u8;
    if unsafe { mbsid_bank_patch_info(bank, patch, &mut eng, &mut vfl) } == 0 {
        Some((eng, vfl))
    } else {
        None
    }
}

// --- host stubs (non-riscv32, e.g. x86_64 for cargo test --lib) ---

#[cfg(not(target_arch = "riscv32"))]
static HOST_REGS_STUB: [u8; 32] = [0u8; 32];

#[cfg(not(target_arch = "riscv32"))]
pub fn init() {}

#[cfg(not(target_arch = "riscv32"))]
pub fn tick() -> bool { false }

#[cfg(not(target_arch = "riscv32"))]
pub fn regs_l() -> &'static [u8; 32] { &HOST_REGS_STUB }

#[cfg(not(target_arch = "riscv32"))]
pub fn regs_r() -> &'static [u8; 32] { &HOST_REGS_STUB }

#[cfg(not(target_arch = "riscv32"))]
pub fn load_patch(_buf: &[u8; 512]) -> bool { true }

#[cfg(not(target_arch = "riscv32"))]
pub fn program_change(_patch: u8) {}

#[cfg(not(target_arch = "riscv32"))]
pub fn note_on(_chn: u8, _note: u8, _vel: u8) {}

#[cfg(not(target_arch = "riscv32"))]
pub fn note_off(_chn: u8, _note: u8) {}

#[cfg(not(target_arch = "riscv32"))]
pub fn pitch_bend(_chn: u8, _bend14: u16) {}

#[cfg(not(target_arch = "riscv32"))]
pub fn cc(_chn: u8, _cc: u8, _val: u8) {}

#[cfg(not(target_arch = "riscv32"))]
pub fn aftertouch(_chn: u8, _val: u8) {}

#[cfg(not(target_arch = "riscv32"))]
pub fn bank_count() -> u8 { 1 }

#[cfg(not(target_arch = "riscv32"))]
pub fn bank_load(_bank: u8, _patch: u8) -> bool { true }

#[cfg(not(target_arch = "riscv32"))]
pub fn bank_patch_name(_bank: u8, _patch: u8, buf: &mut [u8; 17]) {
    // Deterministic placeholder for host tests/builds.
    let name = b"HOST STUB PATCH\0";
    buf[..name.len()].copy_from_slice(name);
}

#[cfg(not(target_arch = "riscv32"))]
pub fn bank_patch_info(_bank: u8, _patch: u8) -> Option<(u8, u8)> {
    Some((0, 0)) // Lead, Mono
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_stubs_are_sane() {
        assert!(bank_count() >= 1);
        assert!(bank_load(0, 0));
        let mut buf = [0u8; 17];
        bank_patch_name(0, 0, &mut buf);
        assert!(buf.iter().any(|&c| c != 0), "name stub must be non-empty");
        assert_eq!(buf[16], 0, "buffer must stay NUL-terminated");
    }

    #[test]
    fn bank_patch_info_stub_returns_lead_mono() {
        let info = bank_patch_info(0, 0);
        assert!(info.is_some(), "stub must return Some");
        let (eng, vfl) = info.unwrap();
        assert_eq!(eng, 0, "stub engine must be Lead (0)");
        assert_eq!(vfl, 0, "stub vflags must be 0 (Mono)");
    }
}
