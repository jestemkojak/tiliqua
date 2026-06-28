// mbsid_sys.rs — FFI declarations + safe wrappers for mbsid_shim ABI.
// The extern "C" link block is gated to riscv32 only; host builds use stubs.

#[cfg(target_arch = "riscv32")]
extern "C" {
    fn mbsid_init();
    fn mbsid_load_patch(buf512: *const u8) -> i32;
    fn mbsid_note_on(note: u8, vel: u8);
    fn mbsid_note_off(note: u8);
    fn mbsid_pitch_bend(bend14: u16);
    fn mbsid_cc(cc: u8, val: u8);
    fn mbsid_tick(speed_factor: u8) -> i32;
    fn mbsid_regs_l() -> *const u8;
    fn mbsid_regs_r() -> *const u8;
    fn mbsid_program_change(patch: u8);
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
pub fn note_on(note: u8, vel: u8) {
    unsafe { mbsid_note_on(note, vel) }
}

#[cfg(target_arch = "riscv32")]
pub fn note_off(note: u8) {
    unsafe { mbsid_note_off(note) }
}

#[cfg(target_arch = "riscv32")]
pub fn pitch_bend(bend14: u16) {
    unsafe { mbsid_pitch_bend(bend14) }
}

#[cfg(target_arch = "riscv32")]
pub fn cc(cc: u8, val: u8) {
    unsafe { mbsid_cc(cc, val) }
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
pub fn note_on(_note: u8, _vel: u8) {}

#[cfg(not(target_arch = "riscv32"))]
pub fn note_off(_note: u8) {}

#[cfg(not(target_arch = "riscv32"))]
pub fn pitch_bend(_bend14: u16) {}

#[cfg(not(target_arch = "riscv32"))]
pub fn cc(_cc: u8, _val: u8) {}
