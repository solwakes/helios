# Rust on Helios: `std`, Targets, and the Porting Path

*Status: Strategy A (`helios-std`) shipped in M31. Strategies B and C remain future work. Updated 2026-04-17.*

Rust's `core` + `alloc` are OS-independent and port to Helios freely. `std` is the problem: it bakes in POSIX metaphors throughout. This document maps the path from "Rust programs don't run on Helios" to "Rust is a first-class language for Helios software."

## What Breaks in `std`?

| Module                  | POSIX assumption                           | Helios path forward                          |
|-------------------------|--------------------------------------------|----------------------------------------------|
| `std::fs`               | fd-backed files, paths, read/write/seek    | Map to graph nodes with caps                 |
| `std::process::Command` | exec, argv, stdio pipes, exit codes        | Map to task spawning, typed IPC              |
| `std::env`              | argv, env vars, cwd                        | Task-local subgraph                          |
| `std::net`              | BSD sockets (fd-backed)                    | Natural fit: existing Helios TCP API         |
| `std::thread`           | Kernel threads with POSIX semantics        | Needs scheduler support (mostly there)       |
| `std::sync`             | Futex-backed mutexes                       | Needs `futex`-equivalent syscall             |
| `std::time`             | Monotonic + wall clock                     | Easy: Helios has both                        |
| `std::path`             | POSIX path separator/semantics             | Mapping to graph traversal                   |
| `std::io::Read/Write`   | Byte-stream abstraction                    | Works fine on node content                   |

`core` + `alloc`: **port freely.** `std`: **requires decisions.**

## Three Strategies

### Strategy A: `helios-std` — The Rust-Native "libc" (**Shipped in M31**)

`helios-std` is a Helios-native stdlib, positioned explicitly as **what you link against instead of libc when targeting Helios**. It is not a `std`-alike; it is the Rust-native equivalent of libc without POSIX baggage.

Every Helios-native Rust program links `helios-std` as its primary dependency. This is the **default target** for new Helios software. Other strategies exist for compatibility, but helios-std is what you reach for first.

**Shipped in M31** (see `crates/helios-std/`):

- **Raw syscall bindings** (`helios_std::sys`) — `syscall0/1/2/3` + per-syscall wrappers for the M30 ABI (`SYS_READ_NODE`, `SYS_WRITE_NODE`, `SYS_LIST_EDGES`, `SYS_FOLLOW_EDGE`, `SYS_SELF`, `SYS_PRINT`, `SYS_EXIT`).
- **Typed graph primitives** (`helios_std::graph`) — `NodeId` newtype, `Label` enum (`Read`/`Write`/`Exec`/`Traverse`/`Unknown(u8)`; aliased as `LabelKind` to match the doc vocabulary), `EdgeInfo` struct (aliased as `Edge`), `Errno` (`Perm`/`NotFound`/`Invalid`/`Other`). Wrappers: `read_node`, `write_node`, `list_edges` (returns `Vec<Edge>`), `list_edges_into` (zero-alloc variant), `follow_edge` — all returning `Result<_, Errno>`.
- **I/O** (`helios_std::io`) — `print` / `println` byte-oriented helpers + `Stdout` implementing `core::fmt::Write`; plus `#[macro_export] macro_rules! println` so user code gets the familiar syntax. Output goes through `SYS_PRINT` (UART today, framebuffer/net later when capped).
- **Task primitives** (`helios_std::task`) — `self_id()` (via `SYS_SELF`), `exit(code)` (via `SYS_EXIT`), `args()` returning the two `usize` values the kernel passed at entry (M31 stand-in for proper `argv`/`env`).
- **Global allocator** (`helios_std::heap`) — a 64 KiB bump allocator backing `alloc::String` / `alloc::Vec` / `alloc::format!`. The arena lives *inside the binary image* (in `.data.helios_heap`), because the kernel currently maps exec edges as R+W+X and no `SYS_MAP_NODE`-style page-granting syscall exists yet. No free; lifetime is the task.
- **Runtime glue** — the `helios_entry!` macro expands to `_start` (placed in `.text.entry` via linker script so it lands at `0x4000_0000`) which stashes the kernel-passed `a0`/`a1`, calls `main()`, and `SYS_EXIT(0)` on return. Also emits a `#[panic_handler]` that prints the panic message and `SYS_EXIT(1)`.
- **Prelude** (`helios_std::prelude`) — one-stop glob import: `NodeId`, `Edge`, `EdgeInfo`, `Label`, `LabelKind`, `Errno`, `self_id`, `exit`, `read_node`, `write_node`, `list_edges`, `follow_edge`, `print`, `println`, plus `alloc::{Box, String, Vec, format, vec}`.

First consumer: `crates/hello-user/` (`spawn hello` in the shell), a ~72 KiB native-Rust user binary that prints, self-introspects via `list_edges`, deliberately trips `Errno::Perm` on an unauthorised `read_node`, and shows the heap high-water mark — proving every piece of the stack lights up end-to-end.

What it does NOT provide:
- POSIX file descriptors (use node IDs)
- POSIX paths (use graph traversal)
- POSIX fork/exec (use task spawning via graph)
- Any implicit ambient authority (every syscall goes through cap checks)

**Recommended for:** all Helios-native Rust software — the toolkit, user-mode apps, eventually helios-native networked services. **This is the default choice for new Rust-on-Helios work.**

Pros:
- Clean, graph-native, no POSIX leakage
- Small, focused API surface
- Compatible with `no_std` Rust crates via `core` + `alloc`
- Simpler to implement than porting `std`
- Evolves freely with Helios — not chained to upstream `std`

Cons:
- Crates that use `std::fs` / `std::process` / `std::net` can't be consumed as-is (bridge via Strategy B or C if needed)
- Unfamiliar to Rust developers who expect `std`

#### M31 caveats — stopgaps that follow-on milestones will lift

- **Bump allocator inside the binary.** Until a `SYS_MAP_NODE`-style syscall exists, the heap lives in the user image's `.data` section. That means (a) 64 KiB arena cap, (b) no free, (c) a long-lived task eventually exhausts its heap. First follow-on: add a `SYS_MAP_NODE` that grants a zeroed page under a newly-created `write` edge, and swap the bump allocator for a real free-list.
- **W^X waived at the task level.** The kernel currently maps exec edges as R+W+X+U because the Rust binary's `.data` lives in the same image as its `.text`. Cross-task caps remain strictly MMU-enforced (no edge → no mapping). A future "split image" approach — one `text` edge for `.text`/`.rodata`, one `rwdata` edge for `.data`/`.bss` — can restore W^X without asking the linker to know the boundary.
- **No argv / no env.** Tasks receive two `usize` registers (`a0`, `a1`) at entry — accessible via `helios_std::task::args()`. A graph-native "spawn context" (a node whose edges describe what the task should operate on) is the right long-term answer; M31 ships the minimal stand-in.
- **`list_edges` ceiling of 256 entries per call.** The kernel has no offset/continuation argument yet, so a node with more edges is silently truncated. A paged variant is tracked with the `SYS_MAP_NODE` work.
- **Static TLS, signals, threads: not there.** U-mode is still single-hart, single-task. No thread API in helios-std yet.

#### Cargo ergonomics (what actually lands on disk)

The kernel and userspace live in separate Cargo workspaces. Root `Cargo.toml` keeps only the kernel crate; `crates/Cargo.toml` is a sub-workspace with `helios-std` (rlib) and `hello-user` (bin). The split exists because user binaries use a different linker script (`crates/hello-user/linker.ld`, origin `0x4000_0000`) than the kernel (`src/arch/riscv64/linker.ld`, origin `0x8020_0000`) — and Cargo's `target.<triple>.rustflags` are *concatenated* across the cwd-to-$HOME config walk, not replaced, so a per-workspace isolation is the only clean way to keep those linker scripts from cross-contaminating. The kernel's own link args are emitted from `build.rs` via `cargo:rustc-link-arg=` for the same reason.

Build pipeline: `cargo build --release` at the repo root runs kernel `build.rs`, which shells out to `cargo build --release -p hello-user` in `crates/`, then `riscv64-elf-objcopy -O binary` on the resulting ELF, then `include_bytes!` pulls the raw blob into the kernel at compile time. `spawn hello` creates a fresh task node, adds an `exec` edge to the hello-user code node plus a `traverse` self-edge, drops to U-mode, and collects the exit code.

### Strategy B: `riscv64-helios` Rust Target

Add Helios as a rustc target. Implement `std`'s internal `sys` layer (`std::sys::helios`) on top of Helios syscalls directly. `std::fs::File::open("/devices/framebuffer")` becomes a graph traversal + cap check + `MAP_NODE` syscall.

This requires implementing:
- `std::sys::helios::fs` — File, Metadata, DirBuilder, ReadDir
- `std::sys::helios::process` — Command, Child, ExitStatus, Stdio
- `std::sys::helios::net` — TcpStream, TcpListener, UdpSocket
- `std::sys::helios::thread` — Thread, park/unpark
- `std::sys::helios::mutex` — Mutex, Condvar, RwLock
- `std::sys::helios::env` — args, vars, current_dir, current_exe
- `std::sys::helios::time` — Instant, SystemTime
- `std::sys::helios::path_prefix_parser` — path component parsing
- `std::sys::helios::os_str` — OsString encoding

Rough size: thousands of lines. Tracked upstream forever. Target gets maintenance burden.

Pros:
- `cargo build --target riscv64-helios` Just Works for well-behaved Rust crates.
- Programs can use `std` for portability *and* `helios::*` for native features in the same codebase.
- Type information flows through `std` APIs (a `File` knows its backing node ID).

Cons:
- Big engineering commitment.
- Needs upstream cooperation (or a permanent fork).
- Some `std` APIs genuinely don't map (e.g. process groups, setuid, fork-then-exec semantics).

**Recommended for:** making the Rust ecosystem available on Helios broadly, once the core kernel ABI is stable (post-M32).

### Strategy C: POSIX Shim as Rustc's libc

rustc compiles for a target whose libc is `helios-libc` (see [porting.md](porting.md)). std uses libc as normal. Every Rust program "just works" modulo libc caveats.

This is the cheapest path to "ship random Rust binaries on Helios." It's also the path most likely to make Helios feel like Unix-with-extras — the Rust program sees POSIX, nothing graph-flavored leaks upward.

Pros:
- Minimal std engineering — just a Rust target that uses our libc.
- Maximum ecosystem compatibility.

Cons:
- Rust programs never see the graph. Typed edges, caps, reactivity are invisible through the shim.
- Easy path = default path. Risk of the ecosystem calcifying around Strategy C.

**Recommended for:** bringing a specific legacy Rust binary onto Helios (e.g. a tool that already exists and you just want running), not for writing new Helios software.

## The Triple Coexistence

A mature Helios Rust ecosystem has all three, with a clear primary:

- **`helios-std`** — the default. The Rust-native libc for Helios. Most native software lives here.
- **`riscv64-helios` target** — for portable Rust crates that want to work on Helios without rewriting to `helios-std`. Lets you pull in commodity `no_std`-or-`std`-using crates (parsers, hashmaps, crypto) with graph-aware primitives available alongside.
- **POSIX shim target** — for specific legacy Rust binaries where rewriting is not practical. Escape hatch, not default.

Libraries bridge: the `helios` core crate exposes `Node`, `Edge`, `NodeId`, `Cap` types, usable from any of the three strategies — so even a Strategy B or C program can reach for graph-native primitives when it wants them.

## `helios-std` First — Where We Are

Strategy A is the priority, and M31 shipped its first cut. Reasons the order was right:

1. We needed concrete examples of native Rust-on-Helios to validate the syscall ABI. Something had to go first.
2. Building a Rust target (Strategy B) without a mature syscall ABI is speculative — the target will change as the ABI settles. Better to let it settle against real programs first.
3. The POSIX shim (Strategy C) depends on `helios-libc` which depends on the syscall ABI, which depends on the experiences from Strategy A.

Revised order:
- **M29–M30 (done):** Syscall ABI — `READ_NODE`, `PRINT`, `EXIT`, then `WRITE_NODE`, `LIST_EDGES`, `FOLLOW_EDGE`, `SELF` + `traverse` cap kind.
- **M31 (done):** `helios-std` crate as described above; `hello-user` as the first native binary.
- **M32 (done):** Graph-native Rust toolkit — `ls <id>` (lists outgoing edges via `SYS_LIST_EDGES`) and `cat <id>` (reads content via `SYS_READ_NODE`). Both live in `crates/{ls,cat}-user/` and link against helios-std. Each target's capability is granted at spawn time by the shell (`traverse` for ls, `read` for cat). These are the first non-trivial graph-native tools — and they validated that the M31 ergonomics carry over: each binary's `main()` is ~50 lines of normal-looking Rust.
- **M33+:** `tree`/`grep`-style tools. A `SYS_MAP_NODE`-style page-grant syscall (lets `helios-std` request fresh R/W pages instead of living inside its binary image), followed by a real heap allocator. Then cap delegation + CDT for revocation. Then `helios-libc` as a Rust library, `riscv64-helios` rustc target, and POSIX shim hardened enough for DOOM-in-U-mode.

## Cargo Considerations

Cargo-built Rust apps (Strategy A-compatible) need:

- `no_std` or `alloc`-only crates: work as-is
- Crates that use `std`: don't compile until Strategy B lands
- Crates with build scripts: need a Helios-compatible cargo feature or environment

A `helios-std` crate + a set of `no_std`-capable dependencies covers a lot. Think `serde_json` (supports `no_std` via `alloc`), `hashbrown` (no_std), core networking crates (with Helios-aware replacements).

Missing entirely for a while: `tokio`, `reqwest`, `actix`. Anything that wants a kernel-thread-pool + POSIX I/O. These require Strategy B or C.

### Vendoring Commodity Crates

For well-solved commodity problems, don't reinvent — vendor. See [porting.md](porting.md#distinctive-vs-commodity) for the distinctive-vs-commodity split and [porting.md](porting.md#selecting-crates-the-ai-ok-filter) for the AI-OK selection filter. Vendored crates live in `vendor/` in the Helios workspace, pinned to specific versions, updated manually.

Candidates known to be likely good fits:
- `hashbrown` (no_std hashmap, from the stdlib)
- `heapless` (no_std data structures)
- `postcard` (no_std serde serializer, wire-format friendly)
- `nom` (parser combinators, works no_std)
- `sha2` / `sha3` / `blake3` (cryptographic hashes)
- `ed25519-dalek` (signatures)
- `serde` + `serde_json` with `alloc` feature

Each is screened against the AI-OK filter before vendoring.

## What a First Native Rust App Looks Like

This is the shape of `crates/hello-user/src/main.rs` as shipped in M31
— a lightly trimmed excerpt of the real source:

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

    match list_edges(me) {
        Ok(edges) => {
            helios_std::println!("my {} outgoing edge(s):", edges.len());
            for e in &edges {
                helios_std::println!("  -> {} [{}]", e.target, e.label);
            }
        }
        Err(e) => helios_std::println!("list_edges failed: {}", e),
    }

    // Deliberate cap violation — prove Errno::Perm propagates through
    // Result rather than panicking. We have no `read` edge to node #1.
    let mut scratch = [0u8; 16];
    match read_node(NodeId(1), &mut scratch) {
        Err(Errno::Perm) => helios_std::println!("read_node(#1) refused — caps work."),
        other            => helios_std::println!("unexpected: {:?}", other),
    }
}
```

and its `Cargo.toml`:

```toml
[package]
name = "hello-user"
version = "0.1.0"
edition = "2021"
build = "build.rs"

[dependencies]
helios-std = { path = "../helios-std" }
```

Shape notes: `#![no_std] #![no_main]`, pulls in `alloc`, imports
`helios-std`, talks to the graph directly. No POSIX leak. The
`helios_entry!` macro emits `_start` and the `#[panic_handler]` so
user code never has to write either; `prelude::*` brings in
`Vec`/`String`/`Box`/`format!` alongside `NodeId`/`Errno`/`Label`.
About 70 KB on disk, most of which is the bump-heap arena.

## Summary

- `core` + `alloc` port freely. `std` is where the work is.
- Strategy A (`helios-std`): **shipped in M31**. Fast path for native software. First priority, first milestone.
- Strategy B (Rust target): enables portable code + graph-native APIs. Heavy engineering, do after ABI settles.
- Strategy C (POSIX shim as libc): cheap compatibility. Last resort for legacy binaries, not the default.
- All three should coexist in the mature ecosystem. Native first, then the compatibility ramp.

---

*Last reviewed: 2026-04-17 (post-M31 helios-std landing). Revisit after M32 (graph-native tools) and again once `SYS_MAP_NODE` lets the bump allocator leave the binary image.*
