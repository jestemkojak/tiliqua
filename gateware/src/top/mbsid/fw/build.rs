// build.rs — cross-compile the vendored MBSID v3 C++ engine into libmbsid.a
// and link it into the riscv32 firmware ELF.
//
// IMPORTANT (Task 4 finding): the "Lead" subset in vendor_sources.txt does NOT
// self-link. `MbSid` aggregates the Bassline/Drum/Multi sound engines + MbSidAsid
// *by value*, so the firmware link needs the FULL engine tree. The full tree DOES
// compile freestanding; we drop the dead non-Lead code at link time with
// --gc-sections (paired with -ffunction-sections / -fdata-sections below).
//
// Host guard: only cross-compile when TARGET is riscv32* so `cargo test --lib`
// on the host stays a pure-Rust build (the FFI is stubbed there, see mbsid_sys.rs).

use std::path::{Path, PathBuf};

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.starts_with("riscv32") {
        // Host build (e.g. x86_64 for `cargo test --lib`): nothing to cross-compile.
        return;
    }

    let mb = PathBuf::from("../mios32/apps/synthesizers/midibox_sid_v3");
    let core = mb.join("core");
    let components = core.join("components");
    let modules = PathBuf::from("../mios32/modules");
    let shim = PathBuf::from("csrc/mios32_shim");

    let includes: Vec<PathBuf> = vec![
        core.clone(),
        components.clone(),
        modules.join("sid"),
        modules.join("notestack"),
        modules.join("random"),
        shim.clone(),
    ];

    let common_flags = [
        "--target=riscv32-unknown-elf",
        "-march=rv32im",
        "-mabi=ilp32",
        "-ffreestanding",
        "-nostdlib",
        "-ffunction-sections",
        "-fdata-sections",
        "-DMIOS32_FAMILY_EMULATION",
    ];

    // ---- C++ engine: ALL of core/*.cpp + core/components/*.cpp (EXCEPT app.cpp,
    //      the mios32 firmware main) + our extern "C" shim. -----------------------
    let mut cxx = cc::Build::new();
    cxx.cpp(true).compiler("clang++").cpp_link_stdlib(None);
    for f in &common_flags {
        cxx.flag(f);
    }
    cxx.flag("-fno-exceptions")
        .flag("-fno-rtti")
        .flag("-fno-threadsafe-statics")
        .flag("-fno-use-cxa-atexit");
    for inc in &includes {
        cxx.include(inc);
    }

    let mut n_cpp = 0;
    for dir in [&core, &components] {
        for entry in std::fs::read_dir(dir).expect("read core/components dir") {
            let p = entry.unwrap().path();
            if p.extension().and_then(|e| e.to_str()) != Some("cpp") {
                continue;
            }
            let name = p.file_name().unwrap().to_str().unwrap();
            if name == "app.cpp" {
                continue;
            } // mios32 firmware main — not ours
            cxx.file(&p);
            n_cpp += 1;
        }
    }
    cxx.file("csrc/mbsid_shim.cpp");
    n_cpp += 1;
    assert!(
        n_cpp > 20,
        "expected the full engine tree, only found {n_cpp} TUs"
    );
    cxx.compile("mbsid"); // -> libmbsid.a

    // ---- C modules the engine consumes (real implementations, not stubs). -------
    let mut c = cc::Build::new();
    c.cpp(false).compiler("clang");
    for f in &common_flags {
        c.flag(f);
    }
    for inc in &includes {
        c.include(inc);
    }
    c.file(modules.join("sid/sid.c"))
        .file(modules.join("notestack/notestack.c"))
        .file(modules.join("random/jsw_rand.c"))
        .file("csrc/cxx_runtime.c");
    c.compile("mbsid_c"); // -> libmbsid_c.a

    // ---- Drop the dead non-Lead code pulled in by the by-value aggregation. ------
    // The Rust linker is rust-lld invoked directly (no gcc driver), so pass the
    // GNU-ld flag as-is, NOT wrapped in -Wl,.
    println!("cargo:rustc-link-arg=--gc-sections");

    // ---- Run the C++ engine's static constructors on the target. ----------------
    // riscv-rt's link.x has no `.init_array` output section and its reset path
    // never calls __libc_init_array, so the engine's global ctors (speed factor,
    // RNG seed, clock defaults — see mbsid_shim.cpp) would never run. init_array.x
    // collects `.init_array` + exposes __init_array_start/__init_array_end, which
    // mbsid_run_static_ctors() walks at startup. Found relative to the crate root
    // (cwd at link time), same as memory.x/link.x.
    println!("cargo:rustc-link-arg=-Tinit_array.x");
    println!("cargo:rerun-if-changed=init_array.x");

    // ---- Rerun triggers. --------------------------------------------------------
    println!("cargo:rerun-if-changed=csrc");
    rerun_dir(&core);
    rerun_dir(&components);
    println!(
        "cargo:rerun-if-changed={}",
        modules.join("sid/sid.c").display()
    );
}

fn rerun_dir(dir: &Path) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            println!("cargo:rerun-if-changed={}", e.path().display());
        }
    }
}
