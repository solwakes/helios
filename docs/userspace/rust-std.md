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

### Strategy A: `helios-std` — The Rust-Native "libc" (Primary Target)

Ship a Helios-native stdlib, `helios-std`, positioned explicitly as **what you link against instead of libc when targeting Helios**. It is not a `std`-alike; it is the Rust-native equivalent of libc without POSIX baggage.

Every Helios-native Rust program links `helios-std` as its primary dependency. This is the **default target** for new Helios software. Other strategies exist for compatibility, but helios-std is what you reach for first.

What it provides:
- **Syscall bindings** — raw and typed wrappers for `SYS_READ_NODE`, `SYS_WRITE_NODE`, `SYS_LIST_EDGES`, `SYS_FOLLOW_EDGE`, etc.
- **Graph primitives** — `Node`, `Edge`, `NodeId`, `Cap` types mapping directly to kernel concepts
- **Capability helpers** — check/request/delegate caps, walk one's own outgoing edges
- **I/O** — node-content streams (not file streams), UART/framebuffer access where capped, TCP sockets (mapping the kernel's socket API)
- **Allocator** — global allocator that requests pages from the kernel via `SYS_MAP_NODE` (or similar)
- **Entry/exit** — `#[no_mangle] extern "C" fn _start()`, panic handler, exit code marshalling
- **Core re-exports** — `alloc::String`, `alloc::Vec`, etc. (no `std::` anything)

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
