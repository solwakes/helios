use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let out_dir = env::var("OUT_DIR").unwrap();
    let doom_src = PathBuf::from(env::var("HOME").unwrap())
        .join("projects/doomgeneric/doomgeneric");
    let helios_root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let doom_include = helios_root.join("doom/include");
    let libc_src = helios_root.join("doom/helios_libc.c");

    let gcc = "/opt/homebrew/bin/riscv64-elf-gcc";
    let ar = "/opt/homebrew/bin/riscv64-elf-ar";

    // Common compiler flags
    let cflags: Vec<&str> = vec![
        "-c",
        "-ffreestanding",
        "-nostdlib",
        "-march=rv64gc",
        "-mabi=lp64d",
        "-O2",
        "-mcmodel=medany",
        "-DDOOMGENERIC_RESX=320",
        "-DDOOMGENERIC_RESY=200",
    ];

    // Collect all doom .c source files, excluding platform-specific ones
    let exclude_suffixes = [
        "_sdl.c", "_xlib.c", "_win.c", "_soso.c", "_sosox.c",
        "_allegro.c", "_emscripten.c", "_linuxvt.c",
    ];
    let exclude_prefixes = [
        "i_sdl", "i_allegro",
    ];

    let mut c_files: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(&doom_src).expect("Failed to read doomgeneric dir") {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().map_or(false, |e| e == "c") {
            let name = path.file_name().unwrap().to_str().unwrap();
            if !exclude_suffixes.iter().any(|s| name.ends_with(s))
                && !exclude_prefixes.iter().any(|p| name.starts_with(p)) {
                c_files.push(path);
            }
        }
    }
    // Add helios libc stubs
    c_files.push(libc_src);

    let mut obj_files: Vec<PathBuf> = Vec::new();

    for src in &c_files {
        let stem = src.file_stem().unwrap().to_str().unwrap();
        let obj = Path::new(&out_dir).join(format!("{}.o", stem));

        let mut cmd = Command::new(gcc);
        for flag in &cflags {
            cmd.arg(flag);
        }
        cmd.arg("-isystem").arg(&doom_include);
        cmd.arg("-I").arg(&doom_src);
        cmd.arg(src);
        cmd.arg("-o").arg(&obj);

        let status = cmd.status().expect(&format!("Failed to compile {}", src.display()));
        if !status.success() {
            panic!("Compilation failed for {}", src.display());
        }
        obj_files.push(obj);
    }

    // Archive into libdoom.a
    let lib_path = Path::new(&out_dir).join("libdoom.a");
    // Remove old archive first
    let _ = std::fs::remove_file(&lib_path);

    let mut ar_cmd = Command::new(ar);
    ar_cmd.arg("rcs").arg(&lib_path);
    for obj in &obj_files {
        ar_cmd.arg(obj);
    }
    let status = ar_cmd.status().expect("Failed to run ar");
    if !status.success() {
        panic!("Archiving libdoom.a failed");
    }

    println!("cargo:rustc-link-search=native={}", out_dir);
    println!("cargo:rustc-link-lib=static=doom");
    println!("cargo:rerun-if-changed=doom/");
}
