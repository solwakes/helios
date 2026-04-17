# Post-M32 Directions

*Status: Partially shipped. Written 2026-04-17 after M31 + M32 shipped overnight. Proposal A was implemented as M33 on 2026-04-17, then completed by M33.5 (the GlobalAlloc rerouting follow-on — see the "Shipped in M33.5" note below); Proposal B (sub-option B.2) shipped as M34 the same day. Proposal A is now fully closed. Proposal C is still on the table.*

## Context

M31 (helios-std) and M32 (graph-native `ls` + `cat`) are done. The user-space Rust pipeline works: author a crate under `crates/<name>-user/`, depend on `helios-std`, the kernel's `build.rs` cross-compiles and embeds, `spawn <name> <arg>` runs it under MMU-enforced capabilities.

The M31 + M32 shipping surface the same three gaps over and over. Whatever ships next should close at least one.

## The Three Open Gaps (in priority order)

### Gap 1: No dynamic memory for user tasks

Every user binary carries a 64 KiB bump-heap arena as `[0xAA; 64K]` bytes inside its `.data` section. This works for M31/M32 but has four consequences that compound:

- Every user binary is at least 72 KB on disk (most of which is 0xAA padding).
- `Vec` can't grow past 64 KiB regardless of what the task actually needs.
- There is no free — memory gets reclaimed only on task exit.
- CDT / delegation (Gap 3) needs dynamic edges, and dynamic edges need dynamic pages to back them.

**The fix: `SYS_MAP_NODE`** (or a kernel-granted page-allocation syscall). Details in [Proposal A](#proposal-a-sys_map_node--kernel-granted-user-memory).

### Gap 2: LIST_EDGES label fidelity

`SYS_LIST_EDGES` returns a `u8` kind byte per edge: `0=unknown, 1=read, 2=write, 3=exec, 4=traverse`. Structural edges (`child`, `parent`, `self`, etc.) are all reported as Unknown. `spawn ls 1` shows all 18 root-child edges as `?`.

This is correct per the M30 ABI but dulls the whole "graph is the filesystem" thesis for the user — you can see the shape but not the semantics. Most user programs beyond `ls` (think `find`, `grep`, `which`, `why`) will care about structural labels.

**The fix: `SYS_READ_EDGE_LABEL`** or a format change to return label strings inline. Details in [Proposal B](#proposal-b-list_edges-label-strings).

### Gap 3: No delegation / CDT

Edges are kernel-declared-only at task spawn. There's no way for task A to grant task B a capability at runtime, which means no IPC pattern beyond pre-wired shared nodes, no capability-granting services, no delegation at all. Also no revocation beyond "kill the task."

`docs/design/capability-edges.md` says the standard pattern is a capability derivation tree (CDT): each derived edge carries a back-pointer to its parent, revoking a parent cascades to descendants. This is the seL4 approach and the right shape; it's also subtle to get right.

**The fix: CDT + `SYS_DELEGATE_EDGE` + `SYS_REVOKE_EDGE`.** Details in [Proposal C](#proposal-c-cdt--delegation-m33-in-the-capability-edgesmd-map).

## Three Proposals

### Proposal A: `SYS_MAP_NODE` — Kernel-granted user memory

> **Shipped in M33.** The syscall number is 8; the ABI below was implemented as spec. Notes on what actually landed:
>
> - Kernel side: `src/user.rs` gained `sys_map_node`, `find_free_data_run` (walks the L0 PTEs directly as the "bitmap over 16 slots"), a new `NodeType::Memory` variant in `src/graph/mod.rs`, and per-task `mem_node_ids` tracking so memory nodes are removed on task exit. See "M33 Implementation Notes" in `docs/design/capability-edges.md`.
> - helios-std: `sys::SYS_MAP_NODE`, `sys::ENOMEM`, `sys::sys_map_node`, `graph::Errno::NoMem`, `graph::map_node`, `graph::map_node_slice`. Re-exported from `prelude`.
> - Demo: `crates/mmap-user/` (`spawn mmap`) maps 32 KiB + 8 KiB, fills each with a distinct pattern, verifies readback, checks non-overlap, and proves the two regions are disjoint. UART transcript at `screenshots/m33-mmap-uart.txt`.
> - **Not shipped in M33 (deferred intentionally):** rerouting `GlobalAlloc` through `map_node`. The 64 KiB bump heap is still in-binary; the follow-on is to shrink it to 4 KiB and chain `map_node(64 KiB)` slabs. This kept M33 scoped to "ship the primitive, unblock downstream work". Cap-model note: `map_node` self-grants `write` (no "grant" cap gates allocation) — matches anonymous `mmap` on Unix; revisit alongside Proposal C.
>
> **Shipped in M33.5 (the heap-integration follow-on).** helios-std's
> `GlobalAlloc` now requests its backing memory from the kernel via
> `SYS_MAP_NODE` rather than embedding a 64 KiB arena in each user
> binary. The allocator is a slab-chained bump allocator: the first
> `alloc` call installs a 16 KiB slab via `graph::map_node`; when a
> request doesn't fit, the allocator requests a larger slab sized to
> the request and keeps bumping. `helios-std::heap::{used, capacity,
> slab_count, SLAB_DEFAULT, MAX_SLABS}` expose allocator stats for
> demo programs. Result: user binaries shrank dramatically
> (`hello-user` from ~72 KiB to ~7 KiB, `mmap-user` from ~70 KiB to
> ~5 KiB) because the `[0xAA; 64K]` padding is no longer in the
> image. The kernel's per-task `mem_node_ids` cleanup reclaims every
> slab on task exit, so no `SYS_UNMAP_NODE` was needed. Accepted
> scope cuts: no per-allocation free (bump semantics), no cross-task
> sharing, no alignment-waste accounting. Demo:
> `crates/bigalloc-user/` (`spawn bigalloc`) allocates a 16 KiB
> `Vec<u64>` then a 32 KiB `Vec<u64>` to force slab chaining and
> prints `list_edges(self_id())` to show two `write` edges to
> `NodeType::Memory` nodes — proof the allocator is backed by real
> kernel-managed memory. Proposal A is fully closed.

**Goal:** A user task can request "give me N bytes of writable memory I own" via syscall. The kernel allocates a fresh node, maps its content pages into the caller's VA, adds a `write` edge from caller → new node. The user sees a pointer to N zeroed bytes.

**ABI sketch:**

```
SYS_MAP_NODE (8)
    a0 = requested size in bytes (will be rounded up to 4 KiB)
    a1 = flags (reserved, pass 0)
    → a0 = user VA of mapped region, or -errno
```

**Under the hood:**

1. Kernel creates a fresh `NodeType::Memory` node in the graph.
2. Allocates `ceil(size/4096)` frames, attaches them to the node's content (or a side-table of anon pages).
3. Adds a `write` edge from caller's task node → new node. This automatically implies `read` per M30 semantics.
4. Extends the caller's page table with R+W+U mappings in the user's data window (`USER_DATA_BASE..USER_DATA_BASE+USER_DATA_MAX_PAGES*4096`).
5. Returns the user VA.

**Edge cases:**

- **Fragmentation of the user VA window.** The M30 `USER_DATA_MAX_PAGES = 16` gives 64 KiB of data space. `SYS_MAP_NODE` lives in that window, as do existing `read`/`write` edge mappings. First call for 4 KiB: easy. After several map+unmap cycles or concurrent edge mappings: need a bitmap / free-list. Start with a bitmap (16 bits = 16 pages; tiny).
- **Double-free / stale pointers.** The user could call `SYS_UNMAP_NODE` then keep using the pointer. Either the kernel invalidates the PT entry (correct; user gets page fault), or we accept UB. Recommend: PT invalidation is cheap, do it.
- **Cross-task mapping.** If task A maps a node and then delegates a `write` edge to task B, both tasks would map it. Out of scope for the initial `SYS_MAP_NODE` — tackle alongside Proposal C.
- **Data vs. heap.** helios-std should decide: use `SYS_MAP_NODE` for `alloc`'s backing memory (shrinking the in-binary arena to ~4 KiB for early-boot allocs, with lazy expansion via `SYS_MAP_NODE` on first overflow), OR expose `map_node(size)` as a public helios-std API and leave the bump heap alone. Former is cleaner; latter is safer for M31 compatibility. Recommend: expose both, let programs opt into the dynamic heap via a helios-std feature flag.

**Estimated effort:** 1 focused milestone. Kernel side ~150 LOC (alloc helper, VA bitmap, syscall dispatch case). helios-std side ~80 LOC (`sys::map_node` + optional `GlobalAlloc` backend). 1 new user-space utility (`gmap` that demos it) ~50 LOC. Docs + tests ~30 LOC.

**Risks:**
- VA fragmentation is easy to get wrong. Start with a simple bitmap.
- If the fresh-node allocation mechanism diverges from the current `graph::create_node`, the thesis wobbles. Keep it as a normal graph op.
- Need to decide what happens when a task exits with `SYS_MAP_NODE` nodes dangling. Recommend: task exit walks the task's outgoing `write` edges to Memory-type nodes, deletes them + their frames. Explicit in the code; not magical.

### Proposal B: `LIST_EDGES` label strings

> **Shipped in M34 (sub-option B.2).** Added `SYS_READ_EDGE_LABEL`
> (syscall 9) — the append-only variant. `SYS_LIST_EDGES` kept its
> 16-byte-per-entry ABI; callers that want the structural label string
> issue one follow-up syscall per edge. helios-std exposes
> `graph::read_edge_label(src, idx) -> Result<String, Errno>` (with
> a zero-alloc `read_edge_label_into` companion). `ls-user` now calls
> it on `Label::Unknown` edges and prints the actual string
> (`child`, `parent`, …) instead of the `?` placeholder.
>
> B.2 won over B.1 for three reasons, all validated during the ship:
>
> 1. Append-only — no kernel or helios-std binary breakage, all M33
>    callers (`who`, `explorer`) Just Keep Working.
> 2. Pay-as-you-go — `who` and `explorer` never needed the string, so
>    they pay nothing. Only `ls` issues the extra syscalls.
> 3. Future-proofed without locking in a format — if the N+1 cost ever
>    bites, a `SYS_LIST_EDGES_V2` can widen the entry later; the
>    current ABI stays valid.
>
> The full rationale + edge cases (buffer-growth retry, indexing
> stability, cap surface) live in `docs/design/capability-edges.md`
> under "M34 Implementation Notes".

**Goal:** `ls` can print `child`, `parent`, etc. instead of `?` for structural edges.

**Two ABI options:**

**B.1: Widen the entry format.** Change `SYS_LIST_EDGES` to return 32-byte entries instead of 16:

```
offset  type          meaning
 0      u64           target node id
 8      u8            label kind (unchanged)
 9      u8[3]         pad
12      u32           offset to label string (from buffer start)
16      u8[16]        scratch / future (zero)
```

Kernel appends label strings as NUL-terminated UTF-8 after the last entry.

- Pro: one syscall; no round-trip per edge.
- Con: ABI break. Existing callers (`ls-user`) need to migrate. Buffer size grows ~4x.

**B.2: Add `SYS_READ_EDGE_LABEL`.** New syscall that takes `(src_id, edge_index)` and returns the label string into a user buffer.

```
SYS_READ_EDGE_LABEL (9)
    a0 = src node id
    a1 = edge index (0-based, same ordering as LIST_EDGES)
    a2 = buf pointer
    a3 = buf len
    → a0 = bytes written (or -errno)
```

- Pro: append-only; doesn't break M30 callers. Optional syscall — `ls` only calls it when the kind is Unknown and the user wants labels.
- Con: N+1 syscalls instead of 1 — slower for `ls` on many-edge nodes.

**Recommend B.2** for the minimum-change path. The perf difference is trivial at M33 scale (no node has >100 edges in practice).

**Estimated effort:** 1 small milestone (or a slice of M33 alongside Proposal A). Kernel ~40 LOC. helios-std ~30 LOC. `ls-user` update ~20 LOC.

**Risks:** trivial. Pure additive syscall with an obvious cap check (`traverse` on src).

### Proposal C: CDT + delegation (M33 in the capability-edges.md map)

**Goal:** Task A can grant one of its edges to task B, revoke it later, and revocation cascades correctly.

**Data model:** each edge gains an optional `derived_from` field pointing at the parent edge (the edge A copied from). Revoking edge E:

1. Walk all edges in the graph whose `derived_from` == E or a transitive descendant (a BFS over the reverse of the `derived_from` relation).
2. Remove all of them atomically (disable the page-table entries first, then free).

This is the capability derivation tree. In graph terms: the tree is recorded as a side-table keyed by edge id (since edges don't currently have ids — they're indexed by `(src_id, position_in_vec)` which is unstable).

**Prerequisite: stable edge identity.** Right now `add_edge` / `remove_edge` shift the indices of other edges on the same src. For CDT to be coherent, edges need stable ids. Options:
- Append-only vec + tombstones (simple, wastes space).
- Swap-remove with a reverse-index side-table (faster, more bookkeeping).

Recommend: append-only + tombstones, iterate skipping tombstones. Profile before optimizing. The M30 "order is insertion order" promise is preserved.

**Syscalls:**

```
SYS_DELEGATE_EDGE (10)
    a0 = edge id (caller must own the source — the task's outgoing edges list)
    a1 = target task node id (must have a `traverse` or similar "hand-caps" cap from caller)
    → a0 = 0 / -EPERM / -ENOENT

SYS_REVOKE_EDGE (11)
    a0 = edge id
    → a0 = count of descendants revoked, or -errno
```

**Cap-to-delegate:** Either any task can delegate any edge it owns (anarchic — every task is a mini-kernel-authority), or delegation itself requires a "grantor" meta-cap. Recommend: **require a grant cap**. Adds `grant` as a fifth edge label, MMU-inert (like `traverse`). A task has `grant` on a target iff it's allowed to delegate caps *pointing to* that target.

This keeps the thesis pure: authority to redistribute authority is itself an edge.

**Edge cases:**
- Delegation loops (A delegates to B, B re-delegates back to A). Already fine — CDT is a DAG of derivations, not a general graph. Each edge has exactly one parent.
- A delegates X to B, then A is killed. CDT currently says X's derived edge in B survives because X wasn't revoked, only the task that held it. Decide: should it die with A? Recommend YES for safety (killing A implicitly revokes A's outgoing edges, which cascades). Document this decision.
- Revocation of self-edges. Can a task revoke its own outgoing edge? Yes, trivially. Degenerate CDT case.

**Estimated effort:** 1 large milestone (several days of careful work). Kernel ~400-600 LOC across `graph/` (stable edge ids, CDT side-table), `user.rs` (new syscalls + page-table invalidation on revoke). helios-std ~100 LOC. Demo programs (grant / revoke / observe) ~150 LOC. Extensive tests — this is the first place where a bug means a real capability leak.

**Risks:**
- Concurrency: revocation during a syscall from a descendant task. Kernel is single-threaded today so this is less bad than it sounds, but the kernel-internal-mutex story needs revisiting when this lands.
- Page-table mutation cost. Revoking a cap requires walking the descendant tasks' page tables and invalidating entries + TLB shootdown. On a single-CPU guest this is a `sfence.vma` per task. On SMP guests (future), this is the first non-trivial TLB shootdown in Helios.
- Testing. Every corner of delegation/revocation wants a targeted test. The existing "run it in QEMU and look" isn't enough.

## Recommendation

**Proposal A (`SYS_MAP_NODE`) shipped as M33.** It unblocks both B (larger edge-list buffers become cheap) and C (dynamic edge creation needs dynamic pages).

Next: **Proposal B** (label strings in `ls`) — it's cheap to build on top of `map_node`.

Then **Proposal C** (CDT) as the first "big" milestone after the utility work. By then helios-std is mature enough that demo programs for delegation/revocation are straightforward to write.

Secondary candidates — worth doing in the white space between the above:

- **`gtree [depth]`** — recursive graph walker. Tests helios-std's `Vec` + recursion. ~100 LOC.
- **`gfollow <src> <label>`** — thin wrapper over `SYS_FOLLOW_EDGE`. Completes the first-pass utility set.
- **`gwrite <id> <content>`** — exercises `SYS_WRITE_NODE`. Demo program for write caps.
- **Host-side unit tests for helios-std.** The pure-data modules (`graph::Label::from_kind`, `Errno::from_raw`, edge serialization) can compile for the host target and be `cargo test`-ed without QEMU. Would catch a surprising fraction of regressions.
- **Kill-orphan-QEMU wrapper script.** Not a milestone, but pain during M31/M32 overnight: parallel `make run` sessions leave zombie qemu-system-riscv64 processes holding the disk lock. A 10-line `scripts/kill-orphans.sh` would save future time.

## Open Questions for Author

These are the decisions I don't want to make unilaterally. Any of these can redirect the whole sequence.

1. **Should `SYS_MAP_NODE` use the existing `USER_DATA_BASE` window, or carve a new window for anonymous memory?** Separating is cleaner but eats more L0 table space. Recommend: shared window with bitmap for M33; revisit if fragmentation gets painful.

2. **On delegation without a `grant` cap, what happens?** Option: anything in `exec` U-mode code can ecall `SYS_DELEGATE_EDGE` for any outgoing edge it owns. Option: require an explicit grant cap. I recommended the latter above but this is a thesis-shaping call.

3. **Should `list_edges` evolve to return the label string (Proposal B.1), or should there be a separate syscall (B.2)?** I recommended B.2 but B.1 might be cleaner long-term.

4. **How much of M33's scope is "ship CDT cleanly" vs. "ship CDT + a demo"?** The first demo of delegation will have a lot of failure modes worth finding. Recommend a scoped demo (task A creates a node, delegates `write` to task B, B writes content, A revokes, B's next write faults). That's ~3 small programs and the kernel bits to support them.

5. **`helios-libc` — should it start now or wait?** `rust-std.md` has it after M33. Waiting keeps focus on the native toolkit, but `helios-libc` is the gate to ports like `busybox`, `vim`, `lua`. No strong reason to rush it — native-first is the thesis — but the question is worth noting.

---

*This doc is a proposal, not a decision. Edit or supersede as needed.*
