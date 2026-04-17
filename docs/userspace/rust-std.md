# Rust on Helios: `std`, Targets, and the Porting Path

*Status: Design sketch. Updated when the first Rust app ships.*

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

### Strategy A: `helios-std` — A Parallel Ecosystem

Ship a Helios-native stdlib, `helios-std`, that's NOT a Rust `std` reimplementation. Programs either link `helios-std` (native) or don't have a stdlib (everything they need from `core`/`alloc`/their own crates).

Pros:
- Clean. No POSIX baggage leaks in.
- First-class graph primitives (`Node`, `Edge`, `Cap`).
- Simpler to implement than a `std` port.

Cons:
- Ecosystem fragmentation. Every Rust crate that uses `std::fs` etc. needs rewriting.
- `cargo`'s happy path is `std` — losing it is a tooling papercut.
- Many great crates can't be used without significant porting effort.

**Recommended for:** core Helios-authored tools (`ls`, `cat`, shell, etc.) written specifically for Helios. First wave of graph-native apps.

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

A mature Helios Rust ecosystem has all three:

- `helios-std` for **native apps** built for Helios (most of the toolkit)
- `riscv64-helios` target for **portable Rust crates** that want to work on Helios without hating their lives
- POSIX shim target for **legacy-Rust binaries** that need to run with minimum porting effort

Libraries bridge: the `helios` crate exposes graph/cap/node types, usable by programs targeting any of the three strategies.

## `helios-std` First

For M29-M32, focus on Strategy A. Reasons:

1. We need concrete examples of native Rust-on-Helios to validate the syscall ABI. Something has to go first.
2. Building a Rust target (Strategy B) without a mature syscall ABI is speculative — the target will change as the ABI settles. Better to let it settle against real programs first.
3. The POSIX shim (Strategy C) depends on `helios-libc` which depends on the syscall ABI, which depends on the experiences from Strategy A.

Order:
- **M29-M30:** Syscall ABI, `helios-std` crate, `ls`/`cat` written against it.
- **M31-M32:** Additional native tools; `helios-libc` crate (as a Rust project — the POSIX shim starts as a Rust library).
- **M33+:** `riscv64-helios` rustc target built on top of the stable ABI. POSIX shim hardened to support real legacy programs.

## Cargo Considerations

Cargo-built Rust apps (Strategy A-compatible) need:

- `no_std` or `alloc`-only crates: work as-is
- Crates that use `std`: don't compile until Strategy B lands
- Crates with build scripts: need a Helios-compatible cargo feature or environment

A `helios-std` crate + a set of `no_std`-capable dependencies covers a lot. Think `serde_json` (supports `no_std` via `alloc`), `hashbrown` (no_std), core networking crates (with Helios-aware replacements).

Missing entirely for a while: `tokio`, `reqwest`, `actix`. Anything that wants a kernel-thread-pool + POSIX I/O. These require Strategy B or C.

## What a First Native Rust App Looks Like

```rust
// crates.io-style Cargo.toml
// [dependencies]
// helios-std = "0.1"

#![no_std]
#![no_main]
extern crate alloc;

use helios_std::graph::{self, Node, NodeId};
use helios_std::caps;
use alloc::string::String;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // Get a handle to our own task node.
    let me = helios_std::task::current();

    // Walk our outgoing edges and print what we can see.
    for edge in graph::edges_of(me) {
        let target = edge.target();
        let Ok(node) = graph::read_node(target) else {
            continue; // no read cap to follow this one
        };
        helios_std::io::println(&String::from_utf8_lossy(node.content()));
    }

    helios_std::task::exit(0);
}
```

Shape: `#![no_std]`, uses `alloc`, imports `helios-std`, writes directly to graph primitives. No POSIX leak anywhere. Minimal dependencies.

## Summary

- `core` + `alloc` port freely. `std` is where the work is.
- Strategy A (parallel `helios-std`): fast path for native software. First priority.
- Strategy B (Rust target): enables portable code + graph-native APIs. Heavy engineering, do after ABI settles.
- Strategy C (POSIX shim as libc): cheap compatibility. Last resort for legacy binaries, not the default.
- All three should coexist in the mature ecosystem. Native first, then the compatibility ramp.

---

*Last reviewed: 2026-04-16 (M28 pre-impl sketch).*
