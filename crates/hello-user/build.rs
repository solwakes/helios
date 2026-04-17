//! Build script: tells `rustc` to use our Helios user linker script.

use std::env;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let linker = manifest.join("linker.ld");
    // Re-link whenever the linker script changes.
    println!("cargo:rerun-if-changed={}", linker.display());
    // Force our own linker script (overrides anything inherited).
    println!("cargo:rustc-link-arg=-T{}", linker.display());
    // Keep .rodata in its own section, don't merge into the text segment.
    println!("cargo:rustc-link-arg=--no-rosegment");
    // Drop dead code/data that was never referenced.
    println!("cargo:rustc-link-arg=--gc-sections");
}
