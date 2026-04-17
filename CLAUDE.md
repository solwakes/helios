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
├── src/                      ← kernel (no_std, riscv64gc)
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
│   └── user.rs               ← (M29+) user-mode task setup, syscall dispatch, embedded user binaries
├── crates/                   ← (M31+) userspace sub-workspace
│   ├── Cargo.toml            ← sub-workspace manifest (excluded from kernel workspace)
│   ├── helios-std/           ← Rust-native "libc" — syscalls, graph types, print, allocator
│   ├── hello-user/           ← first native Rust user program (spawn hello)
│   ├── ls-user/              ← graph-native `ls` (M32) — `spawn ls <id>`
│   ├── cat-user/             ← graph-native `cat` (M32) — `spawn cat <id>`
│   └── mmap-user/            ← SYS_MAP_NODE demo (M33) — `spawn mmap`
├── docs/                     ← design rationale, kernel docs, userspace
├── doom/, doomgeneric/       ← DOOM port (C, linked via FFI)
├── screenshots/              ← milestone screenshots + UART transcripts
├── Cargo.toml / .lock
├── rust-toolchain.toml       ← pinned nightly
├── build.rs                  ← builds doomgeneric + userspace via nested cargo
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

## Milestone Status (M1–M34 Complete)

See `README.md` for the user-facing list. Current front lines:

- **M29 (done):** First U-mode task, MMU cap enforcement, 3 syscalls (`READ_NODE`, `PRINT`, `EXIT`). Pivot to privilege-separation via graph-edge capabilities.
- **M30 (done):** Expanded syscall ABI — `WRITE_NODE`, `LIST_EDGES`, `FOLLOW_EDGE`, `SELF` + the `traverse` capability kind. Four new demos (`who`, `explorer`, `editor`, `naughty`) prove introspection + mutation + write-cap refusal.
- **M31 (done):** `helios-std` — the Rust-native "libc" for Helios user-mode. Raw syscall wrappers (`sys`), typed graph primitives (`NodeId`, `Label`/`LabelKind`, `Edge`/`EdgeInfo`, `Errno`), `print!`/`println!` macros over `SYS_PRINT`, `self_id`/`exit`, a 64 KiB bump allocator for `alloc::*`, and the `helios_entry!` macro that generates `_start` + a panic handler. First native Rust U-mode program lives at `crates/hello-user/`; `spawn hello` runs it. Kernel side: `build.rs` compiles the userspace sub-workspace and embeds the raw binary via `include_bytes!`. Exec edges are now R+W+X (see design notes below) and can span multiple consecutive 4 KiB pages (`USER_CODE_MAX_PAGES = 64`), so real linker-placed Rust binaries sit as one contiguous image at `0x4000_0000`.
- **M32 (done):** Graph-native Rust tools: `ls <id>` walks a node's outgoing edges (`SYS_LIST_EDGES`) and `cat <id>` reads its content (`SYS_READ_NODE`). Live in `crates/ls-user/` and `crates/cat-user/`, both linked against helios-std. Shell grants the task-specific capability (`traverse` for ls, `read` for cat) at spawn time and passes the target id in `a0` — recovered via `helios_std::task::args()`. This is the M31 ergonomics test: can a 50-line Rust `main()` feel like normal Rust while talking directly to the graph? Yes.
- **M33 (done):** `SYS_MAP_NODE` (syscall 8) — a U-mode task can request fresh zeroed writable memory. Kernel mints a `NodeType::Memory` node, allocates backing frames, adds a `write` edge from caller → new node, maps the frames into the caller's data VA window as R+W+U. helios-std exposes `graph::map_node` (returns `NonNull<u8>`) and `graph::map_node_slice` (returns `&'static mut [u8]`); `Errno::NoMem` added for the `-ENOMEM` case. Demo at `crates/mmap-user/` (`spawn mmap`) allocates 32 KiB + 8 KiB and verifies disjoint usable regions. The helios-std `GlobalAlloc` has NOT been rerouted through `map_node` yet — that's a follow-on. See `docs/design/capability-edges.md` "M33 Implementation Notes" for the VA-window / cap semantics / task-exit cleanup specifics.
- **M34 (done):** `SYS_READ_EDGE_LABEL` (syscall 9) — closes the "everything shows as `?`" gap in `SYS_LIST_EDGES`. User programs call it with `(src, edge_idx)` to recover the full UTF-8 label (e.g. `child`, `parent`, `self`) that `LIST_EDGES` only surfaces as kind-byte `unknown`. Append-only ABI (Proposal B.2 from `docs/design/proposals/post-m32-directions.md`) — no existing callers (`who`, `explorer`) broken. helios-std exposes `graph::read_edge_label(src, idx) -> Result<String, Errno>` plus a zero-alloc `read_edge_label_into(src, idx, &mut [u8])`. `ls-user` now prints real labels for structural edges; `spawn ls 1` shows all 19 root-child edges as `child` instead of `?`. See `docs/design/capability-edges.md` "M34 Implementation Notes" for the cap-surface / indexing / buffer-retry rationale.
- **M35+ planned:** Cap delegation with a capability-derivation tree, multiple coexisting user tasks, reroute `GlobalAlloc` through `map_node`, port DOOM to user mode as the litmus test.

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
10. **Nested cargo + rustflags leakage (M31)**: `target.<triple>.rustflags` in `.cargo/config.toml` is **concatenated** across every config file on the cwd-to-$HOME walk, not replaced. Putting the kernel's `-T<linker.ld>` in the root `.cargo/config.toml` leaked into the `crates/` sub-workspace. The fix: emit kernel link args from `build.rs` via `cargo:rustc-link-arg=` (scoped to the kernel crate) and keep `.cargo/config.toml` free of rustflags. See `build.rs` (top) for the pattern.
11. **`static X: T = [0; N]` silently landing in `.bss` (M31)**: `objcopy -O binary` drops NOBITS sections, so a zero-initialised `static` inside a user binary won't make it into the raw blob the kernel copies — subsequent accesses hit a page fault. Force into `.data` by using a non-zero initializer or `#[link_section = ".data.something"]`. See `crates/helios-std/src/heap.rs` for the `[0xAA; N]` pattern.

## Working with Workers

If you're a worker spawned to do Helios work, you don't have the conversation context. Key things to know:

- **Read [`docs/`](docs/) before coding.** Especially [`docs/design/philosophy.md`](docs/design/philosophy.md) for the load-bearing thesis, and any design doc relevant to your task. Don't re-derive decisions that are already written down.
- **Update docs after shipping.** See the "After Shipping Any Feature" section above. README, design docs, and this file all need to reflect what you built. Commit docs in the same commit as code.
- **Don't break the thesis.** The four invariants at the top of this file are load-bearing; if your task seems to require violating one, stop and document why in the relevant design doc before proceeding.
- Always `cargo build --release` before `make run`.
- Network config is fixed: IP 10.0.2.15, gateway 10.0.2.2, hostfwd 5555→80.
- Commit with descriptive messages. Push to main.
- Take a screenshot of the final state and save to `screenshots/m{NN}-{name}.png`.
- For any browser-interactive task, request the `browser-use` capability from the spawning origin — don't fall back to Safari.
- **This repo is PUBLIC.** Commit messages, issue descriptions, and file contents are world-readable. Use neutral language; never attribute design input to specific people by name.

## Before Starting Any Feature

**Read [`docs/`](docs/) first.** Most significant design decisions have already been made and captured:

- [`docs/design/philosophy.md`](docs/design/philosophy.md) — the load-bearing thesis
- [`docs/design/capability-edges.md`](docs/design/capability-edges.md) — security model, syscall ABI, cap labels
- [`docs/design/userspace-tiers.md`](docs/design/userspace-tiers.md) — kernel/native/ported boundaries
- [`docs/userspace/porting.md`](docs/userspace/porting.md) — vendor-vs-write, AI-OK filter, POSIX shim rules
- [`docs/userspace/rust-std.md`](docs/userspace/rust-std.md) — helios-std as primary, rustc target strategy

If the answer isn't in `docs/`, **check whether there's a decision on an adjacent question** — often design is implicit ("we chose X for Y, so Z probably follows"). If still uncertain, write a proposal to the relevant `docs/design/*.md` file *before* coding. Design-first, code-second.

Don't re-derive decisions from scratch. Don't silently assume. Don't skip reading docs because "it's just a small feature." The docs exist so you don't have to rediscover the thesis every time.

## After Shipping Any Feature

**Keep the documentation surface current.** A shipped feature is not done until docs reflect it:

- **[`README.md`](README.md)** — update the "what it has" list if there's a user-visible new capability. Add a screenshot if the feature is visual.
- **[`docs/design/*.md`](docs/design/)** — if the feature implements something the design docs planned, update the design doc's status header and "next steps" / "milestone map" sections. If the implementation revealed a new design decision, document it inline.
- **[`docs/kernel/`](docs/kernel/)** or **[`docs/userspace/`](docs/userspace/)** — when a new kernel subsystem or userspace pattern lands, document it there (create the file if needed). Especially important for ABI changes.
- **This file (`CLAUDE.md`)** — if there's a new gotcha, convention, file structure, or common failure mode, add it to the relevant section (usually "Common Gotchas" or "Key Conventions"). Also update "Milestone Status".
- **Commit docs alongside code**, not in a separate PR. A code commit without doc updates is incomplete.

**If the feature invalidates an earlier design decision**, don't silently change behavior — update the old design doc to reflect the new reality, cite the milestone that changed it, and explain why. Design evolution is allowed; design rewriting-as-if-history-didn't-happen is not.

## Where to Ask Design Questions

If a design decision is significant and not yet documented:
1. Read `docs/design/` first — the answer might already be there under a related topic.
2. If it's new, write a short proposal as a section in the relevant `docs/design/*.md` file.
3. Commit with the proposed change so the reasoning is preserved even if the implementation changes.

Design conversations that happen only in chat get lost. **The repo is source of truth.** When in doubt, write it down, commit it, push it.

---

*Last reviewed: 2026-04-17 (post-M34 `SYS_READ_EDGE_LABEL`).*
