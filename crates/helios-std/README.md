# helios-std

The Rust-native "libc" for Helios user-mode programs (Tier 2 primary
per [`docs/design/userspace-tiers.md`](../../docs/design/userspace-tiers.md)
and [`docs/userspace/rust-std.md`](../../docs/userspace/rust-std.md)).
What you link against instead of `libc` when writing a native Helios
binary.

**Status:** shipped in M31. First user: `crates/hello-user`.

## What it gives you

- **`sys`** — raw `ecall` wrappers (`syscall0`..`syscall3`, `syscall_exit`).
- **`graph`** — typed graph primitives: `NodeId`, `Label`/`LabelKind`,
  `Edge`/`EdgeInfo`, `Errno`. Wrappers around `SYS_READ_NODE`,
  `SYS_WRITE_NODE`, `SYS_LIST_EDGES`, `SYS_FOLLOW_EDGE` that return
  `Result<_, Errno>`.
- **`io`** — `print`/`println`, `Stdout` (implements `core::fmt::Write`),
  `print!`/`println!` macros.
- **`task`** — `self_id()`, `exit(code)`, `args()` (entry-time `a0`/`a1`).
- **`heap`** — a 64 KiB bump allocator installed as the `#[global_allocator]`.
- **`prelude`** — `use helios_std::prelude::*;` for everything at once, plus
  re-exports of `alloc::{Vec, String, Box, format}`.
- **`helios_entry!`** macro — emits `_start` + `#[panic_handler]` in the
  user binary.

The syscall ABI this crate wraps is specified in
[`docs/design/capability-edges.md`](../../docs/design/capability-edges.md).

## Minimal example

```rust
#![no_std]
#![no_main]

extern crate alloc;
use helios_std::prelude::*;

helios_std::helios_entry!(main);

fn main() {
    helios_std::println!("hello from rust userspace!");
    let me = self_id();
    helios_std::println!("my id is {}", me);
}
```

## M31 stopgaps (to be fixed in later milestones)

1. **64 KiB bump heap, no free.** Everything `alloc::*` allocates goes
   into a fixed-size `[u8; 64K]` embedded in the binary image. A
   long-running program will eventually exhaust the arena and all
   allocations start returning null. This is because there's no
   `SYS_MAP_NODE` syscall yet for requesting fresh pages from the
   kernel. Tracking this via `helios_std::heap::used()` /
   `heap::capacity()`.

2. **Heap must live in `.data`, not `.bss`.** `objcopy -O binary`
   drops `.bss` (NOBITS) — if we initialised the arena with `[0; N]`,
   the kernel wouldn't actually copy it into user pages and the heap
   would be unmapped. We use `[0xAA; N]` to force `.data` placement,
   which means 64 KiB of 0xAA bytes in every Helios user binary.

3. **W^X is waived inside a task.** Exec edges are currently mapped
   R+W+X+U so the heap arena (which lives in the same image as code)
   can be written to. Cross-task capability enforcement is unchanged.
   A later milestone may split images into `text` / `rwdata` edges.

4. **No argv/env.** The kernel's spawn interface hands the program
   two `usize` arguments in `a0`/`a1`. `helios_std::task::args()`
   exposes them. A graph-native "spawn context as a child subgraph"
   scheme belongs to a later milestone.

5. **Library cannot emit `_start` or `#[panic_handler]`.** Rust
   forbids these at rlib scope, so `helios_entry!` is a macro the
   user binary invokes to stamp them out at the binary's crate root.

## Where this lives in the repo

- `crates/helios-std/src/` — the library.
- `crates/hello-user/` — the first user binary consuming it.
- `src/user.rs` (kernel) — `build_user_address_space` maps multi-page
  exec edges; `hello_code_id()` / `hello_program_bytes()` expose the
  compiled binary; `run_user_task_with_caps(..., self_traverse=true,
  ...)` spawns it with a traverse-cap back to itself.
- `build.rs` (root) — compiles this sub-workspace via a nested cargo
  invocation, runs `riscv64-elf-objcopy -O binary`, and emits the raw
  bytes to `$OUT_DIR/user-bins/hello-user.bin` for `include_bytes!`
  in `src/user.rs`.

## License

Same as the rest of the Helios project — MIT OR Apache-2.0.
