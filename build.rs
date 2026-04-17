use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let out_dir = env::var("OUT_DIR").unwrap();
    let helios_root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());

    // ------------------------------------------------------------------
    // Kernel link flags — moved out of .cargo/config.toml in M31 so the
    // kernel linker script doesn't leak into the userspace sub-workspace
    // (cargo concatenates `target.*.rustflags` across all configs on the
    // CWD-to-$HOME walk, and rust-lld can't find the kernel linker
    // script when invoked from crates/).
    // ------------------------------------------------------------------
    let linker_script = helios_root.join("src/arch/riscv64/linker.ld");
    println!("cargo:rustc-link-arg=-T{}", linker_script.display());
    println!("cargo:rustc-link-arg=--no-rosegment");
    println!("cargo:rerun-if-changed=src/arch/riscv64/linker.ld");

    // ------------------------------------------------------------------
    // M31: build user-space Rust binaries from crates/ and embed the
    // resulting raw byte blobs for the kernel to load. See
    // `crates/Cargo.toml` for the sub-workspace layout.
    // ------------------------------------------------------------------
    build_user_binaries(&helios_root, &out_dir);

    // ------------------------------------------------------------------
    // Existing: build doomgeneric.c into libdoom.a.
    // ------------------------------------------------------------------
    let doom_src = helios_root.join("doomgeneric/doomgeneric");
    if !doom_src.exists() {
        panic!(
            "doomgeneric source not found at {}. Run `git submodule update --init` to fetch it.",
            doom_src.display()
        );
    }
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

// ----------------------------------------------------------------------
// M31: user-space Rust binary builder.
// ----------------------------------------------------------------------
//
// Compiles the `hello` crate (and any future user binaries) via cargo
// in the `crates/` sub-workspace, then runs `riscv64-elf-objcopy -O binary`
// to produce a raw byte blob at $OUT_DIR/<name>.bin. The kernel picks
// them up with `include_bytes!` in src/user.rs.
//
// Why a sub-workspace shelled out to:
//   - User binaries have their own linker script (crates/hello-user/linker.ld)
//     placing .text at USER_CODE_BASE. The kernel's linker script can't
//     share.
//   - We want isolated build artifacts — kernel and user code don't
//     share a target dir / fingerprint state.
//
// The user crates use the same riscv64gc-unknown-none-elf target as the
// kernel, so the cross toolchain is already installed.

fn build_user_binaries(helios_root: &Path, kernel_out_dir: &str) {
    let crates_dir = helios_root.join("crates");

    // We emit the final .bin files into a well-known path under
    // OUT_DIR so src/user.rs can `include_bytes!` them directly.
    let user_bin_dir = PathBuf::from(kernel_out_dir).join("user-bins");
    std::fs::create_dir_all(&user_bin_dir).expect("failed to create user-bins dir");

    // Target dir under OUT_DIR keeps user-space artifacts separate
    // from the kernel's target/.
    let user_target_dir = PathBuf::from(kernel_out_dir).join("user-target");

    // Build the hello crate.
    //
    // We pass `--manifest-path` + `--target-dir` rather than invoking
    // from crates/ because cargo prints `cargo:` lines on stdout that
    // we must not forward to our own parent — running a nested cargo
    // is the safer path.
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());

    // Build each user-space binary we want to embed into the kernel.
    //
    // Every entry here becomes an `include_bytes!` in src/user.rs and
    // a corresponding `<name>_code_id()` accessor. Adding a new user
    // program is: (1) drop a crate into crates/, (2) list it here,
    // (3) wire `spawn <name>` in src/shell.rs.
    for bin in &["hello-user", "ls-user", "cat-user"] {
        build_user_crate(&cargo, &crates_dir, &user_target_dir, bin, &user_bin_dir);
    }

    // Tell cargo to rerun if anything in the user workspace changes.
    // This isn't perfect (cargo doesn't recurse into directories via
    // rerun-if-changed), but it catches the top-level manifests.
    println!("cargo:rerun-if-changed=crates/Cargo.toml");
    for bin in &["hello-user", "ls-user", "cat-user"] {
        println!("cargo:rerun-if-changed=crates/{bin}/Cargo.toml");
        println!("cargo:rerun-if-changed=crates/{bin}/src/main.rs");
        println!("cargo:rerun-if-changed=crates/{bin}/linker.ld");
        println!("cargo:rerun-if-changed=crates/{bin}/build.rs");
    }
    println!("cargo:rerun-if-changed=crates/helios-std/Cargo.toml");
    println!("cargo:rerun-if-changed=crates/helios-std/src/lib.rs");
    println!("cargo:rerun-if-changed=crates/helios-std/src/sys.rs");
    println!("cargo:rerun-if-changed=crates/helios-std/src/graph.rs");
    println!("cargo:rerun-if-changed=crates/helios-std/src/io.rs");
    println!("cargo:rerun-if-changed=crates/helios-std/src/task.rs");
    println!("cargo:rerun-if-changed=crates/helios-std/src/heap.rs");
    println!("cargo:rerun-if-changed=crates/helios-std/src/prelude.rs");
}

fn build_user_crate(
    cargo: &str,
    crates_dir: &Path,
    target_dir: &Path,
    crate_name: &str,
    out_bin_dir: &Path,
) {
    // Step 1: cargo build --release -p <crate> inside the crates/ workspace.
    let manifest = crates_dir.join("Cargo.toml");
    let status = Command::new(cargo)
        .arg("build")
        .arg("--release")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--target-dir")
        .arg(target_dir)
        .arg("--target")
        .arg("riscv64gc-unknown-none-elf")
        .arg("-p")
        .arg(crate_name)
        // Keep cargo output in stderr so it doesn't pollute the
        // kernel build.rs protocol (stdout lines starting with
        // `cargo:` are directives).
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        // Clear RUSTFLAGS that might leak from the outer cargo
        // invocation — those are kernel-specific (they reference
        // the kernel linker script).
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        // Clear target-dir overrides that the outer cargo may have set.
        .env_remove("CARGO_TARGET_DIR")
        // Stop nested cargo from inheriting our build-script env.
        .env_remove("CARGO_MANIFEST_DIR")
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn cargo for user crate {}: {}", crate_name, e));
    if !status.success() {
        panic!("cargo build failed for user crate '{}' (exit={:?})", crate_name, status.code());
    }

    // Step 2: locate the ELF output.
    let elf_path = target_dir
        .join("riscv64gc-unknown-none-elf")
        .join("release")
        .join(crate_name);
    if !elf_path.exists() {
        panic!(
            "expected user ELF at {:?} after build — cargo layout changed?",
            elf_path
        );
    }

    // Step 3: objcopy -O binary → raw blob.
    //
    // Prefer `riscv64-elf-objcopy` (from homebrew binutils) because
    // it's always the right target and it's already required for the
    // doomgeneric build below. `llvm-objcopy` from rustup would also
    // work, but the binutils one is the one we already depend on.
    let objcopy = "/opt/homebrew/bin/riscv64-elf-objcopy";
    let bin_path = out_bin_dir.join(format!("{}.bin", crate_name));
    let status = Command::new(objcopy)
        .arg("-O")
        .arg("binary")
        .arg(&elf_path)
        .arg(&bin_path)
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn objcopy for {}: {}", crate_name, e));
    if !status.success() {
        panic!(
            "objcopy failed for user crate '{}' (exit={:?})",
            crate_name,
            status.code()
        );
    }

    // Step 4: sanity check — bin should be non-empty and start at
    // offset 0, which is the _start instruction the kernel jumps to.
    let md = std::fs::metadata(&bin_path)
        .unwrap_or_else(|e| panic!("failed to stat {}: {}", bin_path.display(), e));
    if md.len() == 0 {
        panic!("user binary {} is empty — linker script or objcopy broken", bin_path.display());
    }
    println!(
        "cargo:warning=helios-user: built {} ({} bytes) -> {}",
        crate_name,
        md.len(),
        bin_path.display(),
    );
}
