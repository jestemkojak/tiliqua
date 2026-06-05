//! Register access functions for RISC-V system registers.
//!
//! The CSR access uses inline `csrrs`/`csrrw` which only assemble on a RISC-V
//! target. They are gated on `target_arch` so the crate still *compiles* for a
//! host target (e.g. `cargo test --target x86_64-unknown-linux-gnu`), where the
//! bodies become inert stubs. Firmware only ever runs on RISC-V, so the real
//! path is always taken on-target.

macro_rules! read_csr {
    ($csr_number:literal) => {
        #[inline]
        unsafe fn _read() -> usize {
            #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
            {
                let r: usize;
                core::arch::asm!(concat!("csrrs {0}, ", stringify!($csr_number), ", x0"), out(reg) r);
                r
            }
            #[cfg(not(any(target_arch = "riscv32", target_arch = "riscv64")))]
            {
                0
            }
        }
    };
}
pub(crate) use read_csr;

macro_rules! write_csr {
    ($csr_number:literal) => {
        #[inline]
        #[allow(unused_variables)]
        unsafe fn _write(bits: usize) {
            #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
            core::arch::asm!(concat!("csrrw x0, ", stringify!($csr_number), ", {0}"), in(reg) bits);
        }
    };
}
pub(crate) use write_csr;

macro_rules! read_csr_as_usize {
    ($csr_number:literal) => {
        crate::macros::read_csr!($csr_number);

        #[inline]
        #[allow(clippy::must_use_candidate)]
        pub fn read() -> usize {
            unsafe { _read() }
        }
    };
}
pub(crate) use read_csr_as_usize;

macro_rules! write_csr_as_usize {
    ($csr_number:literal) => {
        crate::macros::write_csr!($csr_number);

        #[inline]
        pub fn write(bits: usize) {
            unsafe { _write(bits) }
        }
    };
}
pub(crate) use write_csr_as_usize;
