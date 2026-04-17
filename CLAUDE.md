# CLAUDE.md — For AI Agents Working on Helios

This file is a fast orientation for AI agents (Claude, Sol, or anyone else) contributing to Helios. Read [`docs/`](docs/) for deep design rationale; this file is about **getting productive quickly** and **not breaking the thesis**.

## What Helios Is (Two Sentences)

Helios is a RISC-V 64-bit OS in Rust where the primary data structure isn't a filesystem — it's a persistent, typed graph. Processes, devices, files, and IPC channels are all nodes; labeled edges encode both connectivity and (as of M29) capability-based security.

## The Thesis Is Load-Bearing

Before making any significant architectural choice, check whether it violates one of these:

1. **The kernel only speaks graph ops + caps.** No POSIX. No file descriptors in the kernel ABI. No paths as first-class kernel concept. See [`docs/design/philosophy.md`](docs/design/philosophy.md).
2. **Capabilities ARE edges.** No separate capability table, no ACLs, no AppArmor-style profile. A task's outgoing edges define what it can see/do. See [`docs/design/capability-edges.md`](docs/design/capability-edges.md).
3. **POSIX lives in userspace, per-binary.** Helios-libc is a library, not a kernel subsystem. See [`docs/design/userspace-tiers.md`](docs/design/userspace-tiers.md).
4. **The graph describes the graph.** System state is accessible through graph nodes, not special APIs.

If a proposed change violates any of these, either refactor the approach or open a design discussion in `docs/design/`. Don't silently break the thesis.

## Project Layout

```
helios/
├── src/
│   ├── main.rs               ← kmain entry
│   ├── arch/riscv64/         ← boot.S, linker.ld, traps, privilege
│   ├── mm/                   ← page tables, allocator, Sv39 setup
│   ├── graph/                ← node store, edges, reactive content
│   ├── net/                  ← ethernet/ARP/ICMP/TCP/HTTP/JSON
│   ├── virtio/               ← block, net, keyboard, tablet, input drivers
│   ├── shell.rs              ← interactive UART shell
│   ├── navigator.rs          ← framebuffer graph navigator
│   ├── fb/                   ← framebuffer primitives, font, console
│   ├── task.rs / sched.rs    ← task model (nodes) + scheduler
│   └── user.rs               ← (M29+) user-mode task setup, syscall dispatch
├── docs/                     ← design rationale, kernel docs, userspace
├── doom/, doomgeneric/       ← DOOM port (C, linked via FFI)
├── screenshots/              ← milestone screenshots
├── Cargo.toml / .lock
├── rust-toolchain.toml       ← pinned nightly
├── build.rs                  ← builds doomgeneric
├── Makefile                  ← run, run-gui, clean
└── README.md                 ← user-facing intro
```

## Build Commands

```bash
# ALWAYS prepend this before cargo:
export PATH="$HOME/.cargo/bin:$PATH"

# Debug build (fast, unused by make run)
cargo build

# Release build — THIS is what `make run` / `make run-gui` pick up.
# If you edit src/ and then make run without --release, you'll be running STALE code.
cargo build --release

# UART-only run (no framebuffer):
make run

# Framebuffer + keyboard + mouse + net (full GUI):
make run-gui

# Clean build artifacts:
make clean
```

**Critical:** `make run` runs the release binary. `cargo build` builds debug. If you forget `--release`, your changes won't take effect. This has bitten at least two milestones.

## Running + Testing

QEMU GUI appears in a Cocoa window named "QEMU" on macOS. To capture it for screenshots (without bringing it to front or covering other windows):

```bash
screencapture -x -o -l $(~/bin/get_qemu_window_id) /tmp/helios-screenshot.png
```

Network is host-forwarded: `hostfwd=tcp::5555-:80` means host port **5555** → guest port **80**. So:

```bash
curl http://127.0.0.1:5555/nodes     # hits the guest's HTTP server on port 80
nc 127.0.0.1 5555                    # TCP to guest port 80
```

Helios's IP is **10.0.2.15**, gateway **10.0.2.2** (QEMU SLIRP default).

## Key Conventions

### Framebuffer writes

Pixel-by-pixel `volatile_write(u32)` is catastrophically slow under QEMU's ramfb (≈10s per full render). **Always use u64 bulk writes** (two XRGB8888 pixels packed). Existing helper: `fb::fill_rect()`. For custom rendering, load/store in u64s and split into pixel pairs. Discovered M22 framebuffer optimization pass; a full clear went from 10s → ~9ms.

### VirtIO drivers

All VirtIO drivers use `virtio::mmio::init_device_with_features()` to negotiate `VIRTIO_F_VERSION_1` explicitly. The old "accept whatever the device offers" pattern (blk/gpu/input) did NOT work for virtio-net — it silently drops frames if `VIRTIO_F_VERSION_1` isn't negotiated. See `src/virtio/net.rs` init flow for the reference pattern (M24).

### Polling vs interrupts

Every virtio device is polled from the main idle loop in `kmain`. No device uses interrupts yet. If you add a new driver, poll it from the same loop. Interrupts are planned for when we have enough network traffic to justify the complexity.

### Graph mutations

Use the existing `graph::node::*` API. Don't poke the `BTreeMap` directly. System-managed nodes (ID ≤ 15, see `PROTECTED_MAX_ID` in `src/graph/`) should not be mutated from HTTP or user code — they get refreshed by reactive update functions.

### Externally-created nodes

User-space-created content lives under the `/user` node (ID 12). New nodes from HTTP POST go there by default. The side-table `graph::user::registry` tracks source IP + creation uptime per external node. Don't pollute `/system` or other top-level subtrees with external data.

### HTTP routes

Close-after-response, no keep-alive, 4KB request cap, 64KB response cap. Body-reading happens across multiple `tick()` calls via a two-phase state machine (see `src/net/http.rs` — look for `Conn::header_end`). `Cache-Control: no-cache` is set on all responses so polling browsers don't cache.

### TCP sockets

Single-threaded, polling, `static mut` state. Socket table is 16 slots, listener table is 8 slots. Don't hold references across `tcp::*` calls (the kernel pattern is `drop(s)` before any call that might re-enter SOCKETS).

## Milestone Status (M1–M28 Complete)

See `README.md` for the user-facing list. Current front lines:

- **M29 (in progress):** First U-mode task, MMU cap enforcement, 3 syscalls. This is the pivot to privilege-separation via graph-edge capabilities.
- **M30+ planned:** Expand syscall ABI, cap delegation with CDT, multiple coexisting user tasks, port DOOM to user mode as the litmus test.

## Common Gotchas

1. **Forgot `--release`**: `make run` runs stale code. Always `cargo build --release` before `make run`.
2. **Forgot `export PATH="$HOME/.cargo/bin:$PATH"`**: cargo won't be found.
3. **Editing `.git`-sensitive files** from a worker: the knowledge repo at `~/knowledge/` has auto-pull/commit. The Helios repo does NOT — commits are manual. Don't forget to commit + push after shipping a milestone.
4. **Creating new top-level graph nodes at boot**: protect them against `PUT`/`DELETE` by ensuring their ID is ≤ `PROTECTED_MAX_ID`. If you need a higher ID that's still protected, bump `PROTECTED_MAX_ID`.
5. **Breaking the navigator**: the navigator consumes UART input by default. Exit with `q` to drop to a normal shell prompt, then `tty` toggles into framebuffer text console mode if needed.
6. **Overlapping `static mut` borrows**: the kernel uses `static mut` extensively (TASKS, SOCKETS, NODES, ...). Calls that might re-enter these need `drop(s)` first. `#[allow(static_mut_refs)]` is applied in the places that need it.
7. **Missing `fence.i` after mapping executable pages**: when you map new code into a task's page table (M29+), the CPU's icache may hold stale entries. Always `fence.i` after installing executable pages.
8. **Missing `sfence.vma` after SATP change**: required on RISC-V to flush TLB.
9. **Missing `sstatus.SUM = 1` in syscall handlers**: S-mode can't access U-mode memory by default. Set SUM before any copy-to-/from-user.

## Working with Workers

If you're a worker spawned to do Helios work, you don't have the conversation context. Key things to know:

- Follow [`docs/design/philosophy.md`](docs/design/philosophy.md) principles. When in doubt, don't break the thesis.
- Always `cargo build --release` before `make run`.
- Network config is fixed: IP 10.0.2.15, gateway 10.0.2.2, hostfwd 5555→80.
- Commit with descriptive messages. Push to main.
- Take a screenshot of the final state and save to `screenshots/m{NN}-{name}.png`.
- For any browser-interactive task, request the `browser-use` capability from the spawning origin — don't fall back to Safari.

## Where to Ask Design Questions

If a design decision is significant:
1. Read `docs/design/` first — the answer might be there.
2. If it's new, write a short proposal as a section in the relevant `docs/design/*.md` file.
3. Commit with the proposed change so the reasoning is preserved even if the implementation changes.

Design conversations that happen only in chat get lost. The repo is source of truth.

---

*Last reviewed: 2026-04-16 (post-M28, pre-M29 user mode).*
