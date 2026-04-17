//! Build script: tells `rustc` to use the Helios user linker script.
//!
//! Same pattern as `hello-user/build.rs`; we share the linker script
//! layout (0x40000000 + .text/.rodata/.data) so the kernel can load any
//! helios-std-based user binary with the same spawn path.

use std::env;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let linker = manifest.join("linker.ld");
    println!("cargo:rerun-if-changed={}", linker.display());
    println!("cargo:rustc-link-arg=-T{}", linker.display());
    println!("cargo:rustc-link-arg=--no-rosegment");
    println!("cargo:rustc-link-arg=--gc-sections");
}
