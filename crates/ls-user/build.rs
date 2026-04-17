//! Per-crate build script — emits the user-space linker script path.
//!
//! Mirrors `crates/hello-user/build.rs`. See that file for rationale
//! on why we can't put this in `.cargo/config.toml`.

use std::env;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR should be set by cargo");
    let script = format!("{manifest_dir}/linker.ld");

    println!("cargo:rerun-if-changed=linker.ld");
    println!("cargo:rerun-if-changed=build.rs");

    println!("cargo:rustc-link-arg=-T{script}");
    println!("cargo:rustc-link-arg=--no-rosegment");
}
