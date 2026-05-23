# Capability Edges: Graph-Native Security

*Status: Design committed M28, first implementation M29, ABI expanded M30 + M33 + M34 + M35. This document describes the model; implementation details follow as they land.*

## The Core Idea

Helios has a graph. Tasks are nodes in that graph. A task's outgoing edges, labeled with capability tokens (`read`, `write`, `traverse`, `exec`), define exactly what the task can see and do.

**The edges ARE the capabilities.** There is no separate permissions table, no AppArmor profile, no ACL system. The graph structure that IS the OS is also the authority structure.

## Why Not Classical Capabilities?

Systems like **seL4**, **KeyKOS**, and **EROS** already do capability-based security beautifully. They are formally verified in some cases. Why not just do what they do?

The difference: in seL4, capabilities are separate handles — objects stored in capability-spaces (cspaces), addressed by capability-derivation-trees, manipulated via `seL4_CNode_*` ops. Caps are *on top of* the kernel's storage. A task has its cspace; the cspace contains cap handles; the handles point to kernel objects.

In Helios, there is no separate cap space. A task has outgoing edges in the primary graph, and those edges are caps. Same data structure, two uses:
- Connectivity: the edges structure tells you how the system fits together
- Authority: the edges define who-can-do-what

This means:
- **Revocation** = edge removal (an existing graph op)
- **Delegation** = edge copy (an existing graph op)
- **Introspection** = graph traversal (an existing graph op)
- **Enforcement** = MMU page table built from edges (new in M29)

No new concept. No new subsystem. Just "the graph, and a page-table builder that reads it."

## Edge Labels as Capability Tokens

The label on an edge determines what access the edge grants:

| Label       | MMU Mapping    | Semantics                                         |
|-------------|----------------|---------------------------------------------------|
| `read`      | R-only         | Task can map the target node's content as R       |
| `write`     | R/W            | Task can read AND write the target node           |
| `exec`      | R/X            | Task can execute code from the target node        |
| `traverse`  | *not mapped*   | Task can follow this edge via syscall, but can't directly access |

`read`, `write`, `exec` are direct — they correspond to MMU permissions and let the task touch the target's pages without syscall overhead. `traverse` is indirect — the task uses a syscall to *follow* the edge to the target, and the kernel decides what happens next.

A task can have multiple edges to the same node with different labels. `task → framebuffer [write]` alone gives write-only access via MMU. Adding `task → framebuffer [read]` extends it to full R/W.

## Enforcement via MMU

When the kernel schedules a user-mode task:

1. Walk the task's outgoing edges.
2. For each edge to a target node, map the target's content pages into the task's page table with perms matching the edge label.
3. Load the page table (`satp`), flush TLB (`sfence.vma`), drop to U-mode (`sret`).

The task now sees exactly its permitted view. Any access to memory outside that view → page fault → capability violation → task killed (or signaled, eventually).

The MMU does the enforcement. The kernel only has to build the right page table.

## Syscall API (M29 + M30 + M33 + M34 + M35)

The ABI is append-only and numbered; higher numbers were added in later
milestones. M30 introduced the `traverse` capability kind; M33 added
`MAP_NODE` for kernel-granted anonymous writable memory; M34 added
`READ_EDGE_LABEL` to let user programs see structural edge labels
(`child`, `parent`, etc.) that `LIST_EDGES` only reports as `unknown`;
M35 added `DELEGATE_EDGE` / `REVOKE_EDGE` (the CDT runtime — see
"Delegation and Revocation" below) and the fifth cap label `grant`.

| Num | Name               | Args                                                           | Cap check              | Returns                                       |
|-----|--------------------|----------------------------------------------------------------|------------------------|-----------------------------------------------|
| 1   | `READ_NODE`        | `a0`=node_id, `a1`=buf, `a2`=len                               | `read` or `write` edge | bytes read, or -EPERM / -ENOENT               |
| 2   | `PRINT`            | `a0`=buf, `a1`=len                                             | — (bounds-checked)     | bytes printed                                 |
| 3   | `EXIT`             | `a0`=code                                                      | —                      | (no return)                                   |
| 4   | `WRITE_NODE`       | `a0`=node_id, `a1`=buf, `a2`=len                               | `write` edge           | bytes written, or -EPERM / -ENOENT / -EINVAL  |
| 5   | `LIST_EDGES`       | `a0`=src_id, `a1`=buf, `a2`=max                                | `traverse` edge to src | #entries written, or -EPERM / -ENOENT         |
| 6   | `FOLLOW_EDGE`      | `a0`=src_id, `a1`=label, `a2`=len                              | `traverse` edge to src | target_id, or -EPERM / -ENOENT                |
| 7   | `SELF`             | —                                                              | — (always allowed)     | caller's task node id                         |
| 8   | `MAP_NODE`         | `a0`=size_bytes, `a1`=flags (=0)                               | — (self-granting `write`) | user VA of first mapped page, or -EINVAL / -ENOMEM |
| 9   | `READ_EDGE_LABEL`  | `a0`=src_id, `a1`=edge_idx, `a2`=buf, `a3`=buf_len             | `traverse` edge to src | label bytes written, or -EPERM / -ENOENT / -EINVAL |
| 10  | `DELEGATE_EDGE`    | `a0`=target_id, `a1`=to_task_id, `a2`=label_buf, `a3`=label_len | caller holds matching live edge + `grant` to target | new edge's `vec_position` on to_task, or -EPERM / -ENOENT |
| 11  | `REVOKE_EDGE`      | `a0`=target_id, `a1`=label_buf, `a2`=label_len                 | caller holds matching live edge | count of edges cascade-tombstoned, or -EPERM / -ENOENT |

### Cap-check kinds

The syscall layer uses a `has_cap(target, label)` helper that scans the
current task's outgoing edges for an edge with the given label pointing at
`target`. Five labels are recognised today:

- `read`  — grants `READ_NODE` and is an R-only MMU mapping.
- `write` — grants `WRITE_NODE`, auto-implies `read`, and is an R/W MMU mapping.
- `exec`  — grants execution of code from the target, R/X MMU mapping.
- `traverse` — grants `LIST_EDGES` / `FOLLOW_EDGE` from the target; does
  NOT map anything into the task's page table (it's a pure-syscall cap).
- `grant` (M35) — grants `DELEGATE_EDGE` *for caps pointing at* the
  target; MMU-inert (like `traverse`). A task can hold any of
  `read`/`write`/`exec`/`traverse` on T without `grant` on T; in that
  case it can use the cap itself but cannot pass it onward. Per-target,
  not per-(target, label) — see M35 implementation notes for the
  rationale.

### LIST_EDGES entry layout (16 bytes each, little-endian)

```
offset  type    meaning
 0      u64     target node id
 8      u8      label kind: 0=unknown, 1=read, 2=write, 3=exec, 4=traverse, 5=grant
 9      u8[7]   padding (zero)
```

Entries are returned in the order edges were added to the source node
(the graph's `Vec<Edge>` iteration order), with tombstoned edges
(M35) skipped. `FOLLOW_EDGE` also respects this order and returns
the *first* matching edge.

Note: the index a caller sees in a `LIST_EDGES` result is *not* the
edge id; it's a list position over live edges only. Edge ids
(`(src_id, vec_position)`) are an internal kernel-side concept used
by CDT (M35). The wire-format index suffices for `FOLLOW_EDGE` and
`READ_EDGE_LABEL`, both of which iterate live edges in the same
order and re-resolve by position.

Structural labels (`child`, `parent`, `self`, or any other string the
graph stores that isn't one of the four cap kinds) show up as kind-byte
`0` / `unknown`. To recover the full label string, call
`SYS_READ_EDGE_LABEL` with the same 0-based index. See "M34
Implementation Notes" below.

### Copy-in / copy-out bounds

User-space buffers are verified to lie strictly within the user VA
window `[0x4000_0000, 0x4020_0000)` before any kernel read/write. SUM is
already set in `sstatus`, so once the bounds check passes the kernel
dereferences the pointer directly. Out-of-range buffers → `-EINVAL`.

### Still planned

- `APPEND_NODE` (gated on `write` edge)
- `CREATE_NODE`, `ADD_EDGE` — runtime graph mutation from U-mode. CDT
  (M35) added `DELEGATE_EDGE` / `REVOKE_EDGE`, which let a task
  rewire *existing* edges, but a task still cannot mint a fresh node
  or attach an edge to an arbitrary pair of nodes — only the kernel
  does that today (at boot, on `MAP_NODE`, on `DELEGATE_EDGE`). The
  natural cap gate is `grant` on the new edge's *source* node;
  scoping is a future-milestone decision.
- `UNMAP_NODE` / `map_node` free path — M33 has no per-allocation
  reclaim. A region dies with the task.
- ~~Rerouting helios-std's `GlobalAlloc` through `MAP_NODE`~~ —
  shipped in M33.5. Tracked here for historical reference; see
  M33.5 implementation notes below.

## Delegation and Revocation

*Shipped in M35.* The rationale and the model below were the design
target; see "M35 Implementation Notes" further down for what actually
landed.

When task A delegates a capability to task B, A is copying one of its outgoing edges to be outgoing from B. Example: `A → framebuffer [write]` plus `add_edge(B, framebuffer, write)` = `B → framebuffer [write]`.

For proper revocation semantics, we need to know that B's edge is *derived from* A's. Otherwise revoking A's access would leave B's edge orphaned but still valid.

The canonical solution is a **capability derivation tree (CDT)**: each derived edge knows its parent edge. Revoking a parent cascades to all descendants. This is how seL4 handles revocation.

In graph terms: delegated edges get a `derived_from` back-link to the source edge. Revoke source → walk the derivation tree → remove descendants. Task exit also cascade-revokes the dying task's outgoing edges — caps don't outlive the principal that granted them.

CDT semantics shipped in M35, three phases:

1. **Phase 1** (commit `834d23c`) — graph layer: `Edge { live,
   derived_from }`, `EdgeId(src_node_id, vec_position)`, tombstone-based
   stable identity, `Graph::cascade_tombstone` BFS over the
   reverse-`derived_from` relation, ~14 iteration sites updated to
   skip tombstones.
2. **Phase 2** (commit `5591534`) — syscalls: `SYS_DELEGATE_EDGE`,
   `SYS_REVOKE_EDGE`, fifth cap label `grant`, active-task cap-cache
   rebuild + page-table unmap + `sfence.vma` on revoke, task-exit
   cascade.
3. **Phase 3** (commit `c808003`) — litmus: `cdtsmoke-alpha-user` and
   `cdtsmoke-beta-user` exercise the full chain in QEMU, with α's
   baseline verifying β's cascade-on-exit ran cleanly.

The "kernel-declared-only" world that M29–M34 shipped in (no delegation, edges minted only at task spawn) is now historical. Tasks can hand each other authority at runtime, and the kernel cascades revocations through the resulting tree.

## Boot-Time Capability Allocation

At boot, the kernel constructs the initial graph: system nodes, devices, root user directory. User tasks get their initial edges declared by the spawning authority (initially the kernel; eventually, a "init task" that owns the root cap and hands out edges to child tasks).

For M29, a task gets:
- `self [read,write,exec]` — edge to its own task node
- `user_demo_code [exec]` — edge to its code
- One `read` edge to a specific data node

The demo then proves:
- Task can read its permitted data node (success)
- Task tries to read the root node it has no edge to (EPERM — authority violation caught)

## Relationship to Plan 9 and URIs

Plan 9 did "everything is a file + namespaces are per-process". A process's view of the filesystem is a composition of mounted pieces. This is similar in spirit to Helios: each task has a *view* that differs from other tasks.

Difference: Plan 9 namespaces are opaque — they're a flat path namespace that composes filesystems. Helios views are structured — they're graph subgraphs with typed edges. A Plan 9 process can `stat` any path in its namespace; a Helios task can only touch what its edges reach.

Plan 9's namespaces don't enforce; the file server does. Helios edges are enforced by the MMU. This is a significant practical difference — no trusted file server in the TCB for MMU-enforced reads.

## Known Risks

1. **Capability fragmentation.** If every task has a million tiny edges to every little thing it needs, the page tables get huge and build time dominates scheduling. Mitigation: larger-grain "directory" edges that map whole subgraphs.

2. **Cap ambient.** If it's too easy to get new edges, caps become ambient authority (the thing caps were supposed to prevent). Mitigation: explicit cap-granting syscalls, declarative manifests at spawn time.

3. **Revocation complexity.** CDT implementation is subtle. If we get it wrong, caps leak after revocation. Mitigation: careful design, comprehensive tests, probably formal verification for the CDT logic.

4. **POSIX compat subversion.** If a POSIX libc shim gives out broad caps to every ported program "to make things work", we've defeated the purpose. Mitigation: the shim has to earn specific caps from the spawning authority, not receive a global "everything" cap.

## Next Steps (Milestone Map)

- **M29** (done): Skeleton — one U-mode task, MMU enforcement, 3 syscalls. Cap violation = task kill.
- **M30** (done): Expanded syscall ABI — `WRITE_NODE`, `LIST_EDGES`, `FOLLOW_EDGE`, `SELF` + the `traverse` cap kind. Four new user demos (`who`, `explorer`, `editor`, `naughty`) prove introspection + mutation + refusal all work end-to-end.
- **M31** (done): `helios-std` — the Rust-native userspace library. Typed syscall wrappers (`NodeId`, `Label`, `Errno`, `Edge`), `println!`, bump allocator, `_start`/panic-handler glue via `helios_entry!`. First linker-placed Rust U-mode binary (`hello-user`) runs end-to-end with the cap model: `Errno::Perm` propagates through `Result`, and a deliberate `read_node(root)` refusal is observably handled without killing the task. Kernel side: `build_user_address_space` now maps multi-page exec edges (R+W+X+U — W^X inside a task is waived until a follow-on edge-split; cross-task enforcement is unchanged).
- **M32** (done): Graph-native Rust tools — `ls <id>` enumerates outgoing edges (`SYS_LIST_EDGES`), `cat <id>` reads node content (`SYS_READ_NODE`). Both live at `crates/ls-user` / `crates/cat-user`, each a few dozen lines of `match` over `Result<_, Errno>`. Shell grants the exact cap each tool needs (`traverse` for `ls`, `read` for `cat`) and passes the target id as the first task arg. Validates that the M31 ergonomics carry through to real tool-shaped programs.
- **M33** (done): `SYS_MAP_NODE` — kernel-granted anonymous writable memory. Tasks can mint fresh `NodeType::Memory` nodes at runtime; the kernel allocates backing frames, adds a `write` edge from caller → new node (implying `read`), and maps the frames into the task's data-VA window. Demo at `crates/mmap-user/` (`spawn mmap`). See "M33 Implementation Notes" below.
- **M33.5** (done, pure user-space): helios-std's `GlobalAlloc` now back-ends on `SYS_MAP_NODE` instead of a 64 KiB in-binary bump arena. Slab-chained bump allocator; first `alloc` call installs a 16 KiB slab via `graph::map_node`; oversized requests install a slab sized to fit. Each slab appears as a `write` edge from the task to a `NodeType::Memory` node; kernel cleanup at task exit reclaims everything. No kernel changes — all user-space. Demo at `crates/bigalloc-user/` (`spawn bigalloc`) allocates a 16 KiB `Vec<u64>`, then a 32 KiB `Vec<u64>` to force slab chaining, and inspects `list_edges(self)` to verify multiple memory edges exist.
- **M34** (done): `SYS_READ_EDGE_LABEL` — read a single outgoing edge's full UTF-8 label by index. Closes the "everything shows as `?`" gap: `SYS_LIST_EDGES` keeps its compact 16-byte entries with a cap-kind byte, and user code issues one follow-up syscall per structural edge it wants the actual label for. Shipped as append-only (ABI not broken); `spawn ls 1` now prints `child` for all 19 root outgoing edges instead of `?`. See "M34 Implementation Notes" below.
- **M35** (done): Cap delegation + CDT for revocation. Three phases: graph-layer tombstones + `derived_from` lineage (phase 1, commit `834d23c`); `SYS_DELEGATE_EDGE` / `SYS_REVOKE_EDGE` + fifth `grant` cap label + active-task cap-cache + PT invalidation + task-exit cascade (phase 2, commit `5591534`); `cdtsmoke-alpha` / `cdtsmoke-beta` litmus binaries (phase 3, commit `c808003`). Authority is now first-class user-space-mutable. See "M35 Implementation Notes" below.

- **Post-M35 Proposal B** (done): `SYS_UNMAP_NODE` — release a `SYS_MAP_NODE` allocation before task exit. Cap-gated on ownership (`node_id` must be in the calling task's `mem_node_ids`, i.e. self-allocated, not delegated-in). Symmetric to `SYS_MAP_NODE`: zaps PT mappings, cascade-tombstones the task's `write` edge to the node (so any onward delegations also lose access), removes the `Memory` graph node, and flushes the TLB. Backing frames stay resident — frame-level reclaim is still a future milestone, matching the M33 footprint note. Demo at `crates/munmap-user/` (`spawn munmap`). Closes one of three gaps the post-M35 directions doc identified; the other two (multi-task scheduler M36, shared-memory IPC) remain. See "Post-M35 Implementation Notes" below.
- **M36**: Multiple user tasks coexisting. The cross-task cascade-during-run path that M35 phase 2 punted to "post-SMP" lives here.
- **M37**: Port DOOM to user mode (the litmus test — does the cap model handle a big, real program?).

## M30 Implementation Notes

Things worth knowing for M31 and beyond:

1. **Traverse edges are real graph edges, not a separate table.** The
   kernel simply iterates the task's outgoing edges and matches by
   label string. This keeps the thesis intact: everything is in the
   graph.

2. **Self-traverse is explicit.** If a task wants to introspect its own
   edges via `LIST_EDGES`, it needs a `traverse` edge pointing at
   itself. The `explorer` demo gets this at spawn time via
   `run_user_task_with_caps(... self_traverse = true ...)`. Tasks are
   NOT born with self-awareness; it's a cap like any other.

3. **Edge order is insertion order.** `Vec<Edge>` in the node store is
   append-only during edge creation (no re-ordering), so M30's
   "deterministic order" promise is simply "graph storage order".

4. **`WRITE_NODE` replaces content, it does not append.** The node's
   `Vec<u8>` is overwritten wholesale. This is the simplest thing that
   lets the `editor` demo demonstrate "read-modify-write". An explicit
   `APPEND_NODE` is planned.

5. **Each demo blob is one 4 KiB page, PIC, no M extension.** Inline
   `global_asm!` in Rust doesn't inherit the `rv64gc` multi-extension
   set, so the demos use repeated subtraction for decimal itoa rather
   than `divu`/`remu`. Position independence means only `li`, `mv`,
   `ecall`, and PC-relative branches — no `la` or absolute references.
   The user stack at `0x401ff000` is the only writable scratch region.

## M31 Implementation Notes

Things the `helios-std` milestone learned, worth preserving:

1. **Exec edges can span many pages.** `build_user_address_space` now
   walks each `exec` edge's content in 4 KiB chunks (up to
   `USER_CODE_MAX_PAGES = 64`) and lays them out at consecutive VAs
   starting at `USER_CODE_BASE`. A real linker-placed Rust binary
   (text + rodata + data + heap-arena) is one exec edge, one
   contiguous image. The old one-page-per-edge assumption from M29/M30
   still holds for the asm demos — they just use exactly one page.

2. **Exec pages are R+W+X+U in M31.** A Rust binary's `.data` section
   needs to be writable, and emitting two separate edges (one R+X for
   text/rodata, one R+W for data/bss) would require the linker to
   declare where the boundary lives. M31 punts: the whole image is
   R+W+X at the task level. This waives W^X **inside** a task; it
   does not waive cross-task capability enforcement (no edge → no
   mapping → no access). A follow-up milestone can split the image
   into `text` and `rwdata` edges once there's a reason for strict
   W^X intra-task (e.g. JIT hardening).

3. **The bump allocator: in-binary in M31, `SYS_MAP_NODE`-backed from
   M33.5.** M31 shipped a 64 KiB `[0xAA; N]` arena in each user
   binary's `.data` (the non-zero initializer is load-bearing — a
   plain `[0; N]` lands in `.bss` which `objcopy -O binary` drops).
   M33.5 rewired helios-std's `GlobalAlloc` to fetch slabs from the
   kernel via `SYS_MAP_NODE`, shrinking each user binary by ~64 KiB
   and letting `alloc::Vec`/`String` grow up to the task's data-VA
   window (64 KiB in M33). See `crates/helios-std/src/heap.rs` and
   the M33 notes below for the per-slab accounting.

4. **Panic handler + `_start` via macro.** A Rust library cannot define
   `#[panic_handler]`, so `helios-std` provides a `helios_entry!`
   macro that the user binary invokes to emit `_start` + the panic
   handler at the binary's crate root. The `_start` stashes kernel-
   passed `a0`/`a1` into atomic globals so `helios_std::task::args()`
   can retrieve them later (a stand-in for real `argv`/`env` pending
   a graph-native spawn-context scheme).

5. **Cap enforcement works through `Result`.** `hello-user` calls
   `read_node(NodeId(1))` — the kernel root, which it has no `read`
   edge to. The kernel logs the violation and returns `-EPERM`,
   `helios_std::graph::read_node` converts it to `Err(Errno::Perm)`,
   and the demo matches on that path. Importantly the task **is not
   killed**; M29's fault-kill path only triggers on MMU violations
   (direct load/store to an unmapped VA), not on syscall `-EPERM`
   returns. This is what lets graceful "ask forgiveness" patterns work.

## M33 Implementation Notes

Things `SYS_MAP_NODE` learned, worth preserving:

1. **Cap semantics: `map_node` self-grants `write`.** The syscall
   synthesizes the new `Memory` node and then adds a `write` edge from
   the caller's task node → new node. `write` implies `read` under the
   M30 semantics, so the task can also `SYS_READ_NODE` / `SYS_WRITE_NODE`
   the region in addition to touching its pages directly via MMU.
   There is no separate "may I allocate?" cap gating the syscall itself
   — every U-mode task can call `map_node`. That's a deliberate M33
   decision, matching the "a task can always extend itself" model of
   anonymous `mmap(MAP_ANONYMOUS)` on Unix. Gating allocation (e.g. by
   a quota node) is a post-CDT design question.

2. **VA window management: walk the L0 PTEs directly.** The task's
   data-VA window is 16 slots at `USER_DATA_BASE..USER_DATA_BASE +
   USER_DATA_MAX_PAGES*4096` (`0x4010_0000..0x4011_0000`). Rather than
   materialising a separate per-task bitmap, the kernel inspects the
   `PTE_V` bit of each L0 entry on every call to find a contiguous run
   of unused slots. At 16 slots this walk is trivial; a denser structure
   would be over-engineered. `build_user_address_space` marks the exec /
   read / write / stack slots as used by installing leaves; `map_node`
   treats anything with `V=0` as free. See `find_free_data_run` in
   `src/user.rs`.

3. **Task-exit cleanup removes the `Memory` nodes, leaks the frames.**
   `ActiveUserTask.mem_node_ids` tracks every `Memory` node the task
   minted during its run. On exit (or fault), after `ACTIVE = None`,
   the kernel calls `graph::remove_node` on each of those ids — this
   also strips the task→mem `write` edge from the graph. The backing
   frames themselves are not freed; that matches the pre-existing M29
   behaviour for *all* user frames (stacks, read/write edge pages,
   page tables), which also leak on task exit. A proper frame
   reclaim lands with a real page allocator, not as part of M33.

4. **`NodeType::Memory` is a real graph node type.** Added in M33 so
   anonymous memory is visibly distinct from text/binary/config nodes
   in `ls` / the navigator. It serialises (`persist::type_to_u8` → 7)
   and renders (grey in the graph view) like any other type. Keeping
   the thesis pure: "everything is a memory" means even anonymous
   heap is a graph citizen.

5. **No `SYS_UNMAP_NODE` yet.** A task cannot release a region
   before it exits. The syscall is an obvious follow-on — the kernel
   has all the information it needs (node id → L0 entries via the
   frame PA) — but it's not in M33 because (a) there's no demo that
   needs it, and (b) the cleanest API probably takes the `NodeId`, not
   the VA, which requires plumbing a lookup that doesn't exist yet.

## M34 Implementation Notes

Things `SYS_READ_EDGE_LABEL` learned, worth preserving:

1. **Append-only ABI, not a `LIST_EDGES` widening.** The proposal
   (`docs/design/proposals/post-m32-directions.md`, "Proposal B") lined
   up two options: B.1 widening each `LIST_EDGES` entry from 16 bytes
   to 32 to inline label strings, or B.2 adding a separate syscall
   callers use *only* when they want the string. B.2 won: it's
   additive (no user-space churn), zero-cost for callers that already
   act on the cap-kind byte (`who`, `explorer`), and the N+1-syscall
   penalty is irrelevant when `ls` is the only consumer and real nodes
   have <50 edges. If the penalty ever bites, B.1 can ship later under
   a new syscall number (say, `SYS_LIST_EDGES_V2`) without breaking
   existing binaries.

2. **Cap surface matches `LIST_EDGES` exactly.** Both the old syscall
   and the new one gate on `has_cap(src, "traverse")`. The rationale:
   a caller that already saw the edge's target + kind via `LIST_EDGES`
   learns nothing new-in-kind from also seeing its label string.
   Imposing a second cap would be bureaucratic without adding
   authority hygiene.

3. **No NUL terminator; caller interprets the byte count.** The kernel
   returns `label.as_bytes().len()` — exactly what Rust needs to
   slice `&buf[..n]` and decode. helios-std's `read_edge_label` then
   `String::from_utf8_lossy`es the bytes; a future non-Rust caller
   (helios-libc, ported program) can `strnlen`-equivalent on the
   buffer without caring about trailing NUL.

4. **"Buffer too small" is `-EINVAL`, with retry built into helios-std.**
   The kernel refuses to truncate (returns `-EINVAL` when
   `buf_len < label.len()`). `helios_std::graph::read_edge_label`
   starts with a 32-byte stack buffer — enough for every label in the
   current graph — and on `-EINVAL` doubles into a heap buffer up to
   4 KiB. The retry path is never exercised in today's graph but the
   mechanism means callers never silently lose bytes.

5. **Indexing is by `Vec<Edge>` position.** Same ordering rule as
   `SYS_LIST_EDGES`: `edge_index == i` iff you saw the edge as the
   `i`-th entry in the last `LIST_EDGES` result. There are no stable
   edge ids yet (that's part of the Proposal C / CDT work); if another
   task mutates `src.edges` between the two syscalls, the index could
   shift. For read-only inspection from within a single task turn,
   that's fine.

6. **`EdgeInfo` did not grow a label field.** Considered: tacking an
   `Option<String>` onto `EdgeInfo` and populating it lazily inside
   `list_edges`. Decided against for M34: it pushes an allocation
   onto every edge enumeration, even for callers (`who`, `explorer`)
   that never look at the string. A standalone `read_edge_label(src,
   idx)` keeps the cost pay-as-you-go. If a future caller (a graph
   navigator, a `find`-equivalent) wants the strings up-front it can
   build a small `Vec<(EdgeInfo, String)>` in a helper.

## M33.5 Implementation Notes

Things the `GlobalAlloc` rewiring learned, worth preserving:

1. **No kernel change required.** M33.5 is pure user-space —
   helios-std's `GlobalAlloc` is now a client of `SYS_MAP_NODE`
   exactly the way `crates/mmap-user/` is, just invoked implicitly by
   every `Vec::push` / `format!` / `Box::new` / `String::from` that
   flows through `alloc::*`. The kernel's syscall surface, cap model,
   and task lifecycle are unchanged.

2. **Lazy init, no bootstrap arena.** The `_start` shim (expanded by
   `helios_entry!`) only touches `AtomicUsize`s and the panic handler
   only uses stack + `core::write!`, so nothing in the pre-`main`
   path allocates. The first heap allocation `main()` performs is
   what kicks the allocator into requesting its first slab. Dropping
   the old 4 KiB-or-64 KiB in-binary arena shrank `hello-user` from
   ~72 KiB to ~7 KiB on disk.

3. **Slab chain, not a free-list.** Current state is five
   `AtomicUsize` slots in `.data.helios_heap` (`CURRENT_BASE`,
   `CURRENT_END`, `CURRENT_CURSOR`, `SLAB_COUNT`, `PRIOR_BYTES`).
   Only the *current* slab is bumpable; older slabs remain live via
   whatever Rust references still point into them but the allocator
   does not track or reuse them. Deallocation is a no-op; the
   kernel's M33 `mem_node_ids` cleanup at task exit reclaims every
   slab. This is exactly the same "everything lives while the task
   does, nothing lives past it" shape as every other piece of
   per-task state in Helios today.

4. **SeqCst atomics, not Relaxed.** Single-hart U-mode strictly only
   needs `Relaxed`, but an early version using `Relaxed` produced
   stale reads in the fit-retry loop when LTO inlined
   `install_new_slab` into `alloc`: the post-install cursor read
   returned zero. `SeqCst` gave the compiler explicit ordering
   fences and settled the codegen. Cheap at M33.5 scale; revisit if
   the allocator ever shows up in a profile.

5. **No cross-task sharing.** Each user task has its own allocator
   state in its own `.data.helios_heap` (it's a static — no run-time
   coordination between tasks exists yet). A shared-heap design is a
   Proposal C / CDT follow-on, not a M33.5 concern.

6. **Accepted scope cuts.** No per-allocation free. No slab
   compaction. No alignment-waste tracking beyond the conservative
   `PRIOR_BYTES` tally. Oversized allocations request a right-sized
   slab but can still hit `ENOMEM` when the 16-page task data window
   is exhausted — in which case `GlobalAlloc::alloc` returns null
   and Rust panics via `handle_alloc_error`, which is correct OOM
   behaviour.

## M35 Implementation Notes

Things the CDT shipping learned, worth preserving:

1. **Stable edge identity via tombstones, not swap-remove.** Each
   `Edge` gained a `live: bool` field and an
   `Option<EdgeId>` `derived_from` back-link, where
   `EdgeId = (src_node_id, vec_position)`. Removal is now a
   tombstone (`live = false`) — the vec entry stays put, so every
   surviving edge keeps its `vec_position` forever. Alternative
   considered: swap-remove + reverse-index side-table. Rejected for
   simplicity-of-correctness — this is the first place where a bug
   means a real capability leak; the cheapest implementation wins.
   Cost paid in space: tombstones accumulate forever, but at
   typical helios scale (small graphs, infrequent revocations) the
   overhead is irrelevant. Profile before optimising; that was
   Proposal C's explicit guidance and it held.

2. **`Node::iter_live` was the right abstraction.** Phase 1 added
   ~14 iteration-site updates to skip tombstones. Doing
   `for e in node.edges.iter().filter(|e| e.live)` everywhere would
   have been a maintenance hazard; one helper makes the tombstone
   discipline grep-able. Same shape as M30's "self-traverse is just
   another edge" — keep the new concern uniform with the existing
   primitives rather than scattering ad-hoc filters.

3. **Cap-cache rebuild after revoke is just "walk iter_live again".**
   `ActiveUserTask` snapshots edges at spawn-time into five
   `Vec<u64>`s (`read_allowed`, `write_allowed`, `exec_allowed`,
   `traverse_allowed`, `grant_allowed`). After any cascade
   revocation, the simplest correct thing is to rebuild all five
   from scratch by re-walking the active task's `iter_live`. O(edges
   on self), runs once per `SYS_REVOKE_EDGE`, no delta reasoning.
   Cheap at M35 scale; revisit if profiling ever shows it.

4. **Active-task-only PT invalidation.** `SYS_REVOKE_EDGE` is called
   *from* the active task. Single-hart, cooperative — only the
   active task can issue syscalls — so any edge tombstoned by the
   cascade that *also* lives on the active task is the only one
   whose page-table mappings need to come down. Cross-task
   cascade-during-run is structurally impossible on single-hart and
   is explicitly punted to "post-SMP" (M36+). The active task's
   `mappings: Vec<Mapping>` (a clone of `aspace.mappings` extended
   by each `SYS_MAP_NODE` call) drives the unmap; one `sfence.vma
   zero, zero` at end of syscall flushes the full TLB. Selective
   `sfence.vma vaddr, asid` flushes need ASIDs, which Helios doesn't
   use yet.

5. **The fifth label `grant` is MMU-inert and per-target.** Like
   `traverse`, `grant` adds nothing to the page table; it is a
   pure-syscall cap consulted only by `SYS_DELEGATE_EDGE`. Two
   shapes were considered:
   - **per-target** — `grant` on T iff the holder may delegate any
     of its outgoing edges pointing to T. Coarse but simple.
   - **per-(target, label)** — separate `grant_read`, `grant_write`,
     etc., on T. Finer but doubles the cap-label surface.

   Per-target won for M35. The granularity argument: if A wants to
   delegate `read` but not `write` to T, A can simply be granted
   only the `read` edge in the first place — the granularity comes
   from *which edges A holds*, not from which-grant-flavours A
   holds. Matches the rest of the model where edges-are-caps
   rather than caps-have-attributes. Reversible if real workloads
   ever need it.

6. **Cascade-on-task-exit.** When a task exits, the kernel walks
   its outgoing edges and `cascade_tombstone`s each one before the
   existing memory-node cleanup. Anyone the dying task delegated
   to loses their derived caps. Reason: if A could leak caps that
   outlive A, an exited task becomes a permanent shadow authority.
   Principle: caps don't outlive the principal that granted them.
   Kernel-declared boot edges have `derived_from = None`, so the
   kernel-as-principal never exits and root caps never cascade-
   revoke — that's the right shape.

7. **β's invariant verified by α's baseline.** Phase 3's litmus
   test split into two crates: `cdtsmoke-alpha` exercises
   delegate-then-revoke within one task's lifetime;
   `cdtsmoke-beta` exercises delegate-then-exit without explicit
   revoke. β's invariant — that exit cascades the derived edge
   away — is **observable from α**: α's first action is
   `list_edges(B)`, which returns zero, because the edge β
   delegated in a previous run was tombstoned by β's exit. The
   proof of β's invariant lives inside α's output. Registering
   this as a pattern: paired-tests-where-one-verifies-the-other's-
   precondition. Niche shape, but the structural cleanliness
   matters — both binaries together are stronger than either
   alone.

8. **Non-exhaustive match errors as the regression catch.** Adding
   `Label::Grant` (kind byte 5) was a public-enum extension. The
   only unanticipated impl decision during phase 3 was that
   `ls-user`'s `match label { … }` over `Label` was non-exhaustive
   — the compiler surfaced this as an error in five lines of fix.
   Registering: **non-exhaustive match errors are the cheapest
   possible regression catch for public-enum-extends.** Another
   small reason to prefer enums-with-arms over bitfields-with-flags.

9. **Single-hart simplification carried through.** The "active task
   is the only running thing" assumption (cooperative scheduler,
   one user task at a time) is leaned on at three points in M35:
   (a) cap-cache mutation only touches the active task, (b)
   page-table invalidation only happens on the active task, (c)
   cross-task cascade-during-run is impossible. M36 ("multiple
   user tasks coexisting") will need to revisit each. For M35 each
   simplification was free — the right time to think about SMP is
   now, the right time to ship is single-hart.

10. **Edge id encoding: two args, not packed.** `EdgeId = (u64, u32)`
    doesn't fit in one register. Considered packing into one u64
    (limit src to 4G nodes + edge index to 4G — fine in practice
    but a forced future-proofing call) versus passing as two args.
    Passing as two args won — clean ABI, no bit-twiddling, no
    future migration if helios ever exceeds 4G nodes. Both
    `SYS_DELEGATE_EDGE` and `SYS_REVOKE_EDGE` actually take
    `(target_node_id, label_buf, label_len)` rather than raw edge
    ids — the kernel resolves the (caller, target, label) tuple to
    the matching live edge at syscall time. This trades a tiny
    O(edges-on-self) lookup for not having to expose `EdgeId` as
    part of the user ABI.

11. **`SYS_LIST_EDGES_DETAIL` not shipped.** Phase 1 considered
    adding a syscall that returns each live edge's `EdgeId`
    alongside its label byte, for callers that want to drive
    `SYS_REVOKE_EDGE` by id. Decided against for M35: callers
    already know `(target, label)` for any cap they hold, and the
    kernel-side resolve is O(edges-on-self) — cheap. If a future
    caller needs id-stability across `list_edges` calls (e.g. an
    interactive grant editor), `SYS_LIST_EDGES_V2` can ship later
    additively.

12. **Non-goals deliberately deferred.** Several CDT-adjacent
    features were explicitly out of M35: per-allocation free of
    memory nodes (still task-exit-only as of M35; *closed* by
    post-M35 Proposal B — see below); map_node delegation (the
    syscall self-grants `write` but does *not* self-grant `grant`,
    so a task can't onward-delegate its anonymous memory without an
    explicit grant edge); shared-memory IPC primitives. CDT enables
    these; M35 doesn't ship one. The grant-policy note
    (`knowledge/notes/cdt-grant-policy.md`) captured the per-target-
    only-during-spawn decision so the auto-grant-on-map_node option
    (b) stays separable.

13. **Litmus from QEMU, no host-side graph tests yet.** The kernel
    is `no_std` and the project doesn't currently support `cargo
    test` for kernel code (only helios-std has host tests via
    `scripts/test-host.sh`). Phase 3's cdtsmoke binaries cover the
    cascade end-to-end *via QEMU* — single boot, three commands,
    UART transcript captures every observable assertion. A
    follow-on "graph-host-test crate" wrapping the Graph
    primitives is registered as possible future work but isn't
    blocking — the binaries are the integration test M35 was
    always going to need.

## Post-M35 Implementation Notes (Proposal B — `SYS_UNMAP_NODE`)

Proposal B of `docs/design/proposals/post-m35-directions.md` shipped
as a single small piece of M35 vocabulary, after the M35 closeout
landed on 2026-05-12. See that proposal for the gap-surface
discussion this implementation resolves.

1. **Cap-gate is ownership, not a new label.** `SYS_UNMAP_NODE`
   gates on `node_id ∈ active.mem_node_ids` (i.e. allocated-by-self
   via `SYS_MAP_NODE`), not on a new `destroy` cap label. This
   matches the Unix anonymous-`munmap` shape: a task can free its
   own allocations, not the allocations of others. Importantly, a
   task that *only has a delegated `write` edge* to a Memory node
   does **not** appear in its own `mem_node_ids` — Proposal B is a
   per-allocation free, not a graph-node destroy.

2. **Reuses M35 plumbing.** The implementation calls
   `unmap_active_target_pages` (the helper M35 added for
   `SYS_REVOKE_EDGE`'s PT cleanup) and `g.cascade_tombstone` (the
   M35 CDT walk). The net effect is symmetric with `SYS_REVOKE_EDGE`
   in cap terms but with one extra step at the end: `g.remove_node`
   on the Memory node so the freed slot doesn't leak as a
   tombstoned-edge-pointing-at-a-still-extant-node ghost.

3. **Backing frames stay resident.** The M33 footprint note is
   unchanged. Frame-level reclaim depends on a global free pool
   that doesn't exist yet — that's a separate milestone (M37+ class
   of work). The slot in the task's data-VA window *is* released
   and reusable by subsequent `SYS_MAP_NODE` calls; the demo's
   final step verifies the reused VA equals the freed VA.

4. **Cross-task PT cleanup on delegation cascade is single-hart-
   simplified.** If task A unmaps a node that A had delegated
   `write` on to task B, the cascade tombstones B's edge — but B's
   PT mapping is not zapped during the cascade because B isn't the
   current SATP. This is the same M35 simplification described in
   M35 Implementation Notes #9, and lifts the same way once M36
   (multi-task scheduler) lands. Until then, task B's stale PT
   mapping is harmless: B's *cap-cache* doesn't include the freed
   node anymore, so any syscall-mediated access from B will EPERM;
   raw MMU loads from B would read from frames whose contents are
   no longer guaranteed (the node is gone, the frames may get
   handed to a future allocation), but that's the same hazard B
   faces today on any revoke from a non-active source. The fix
   ships with M36's lift of these simplifications, not with B.

5. **Errno shape.** `SYS_UNMAP_NODE` returns `0` on success or
   `ENOENT` on failure. Failure is collapsed into one code because
   the two distinguishable cases (no active task; `node_id` not in
   `mem_node_ids`) can't both happen from U-mode in practice — a
   U-mode syscall has an active task by definition.

6. **Demo (`spawn munmap`).** `crates/munmap-user` allocates A (32
   KiB) and B (8 KiB), reads its own outgoing edges to find their
   `NodeId`s, frees A, verifies that B's edge survives and B's
   mapping still works, verifies a repeat free of A returns
   `NotFound`, and reallocates 32 KiB (C) — verifying C's base VA
   equals A's old base. That last step is the load-bearing slot-
   reclaim assertion: without it the demo would prove only that
   the syscall runs without faulting, not that it actually returns
   the slot to the bitmap.

## Post-M35 Implementation Notes (Proposal A — M36 multi-task scheduler, phase 1.0 plumbing)

Proposal A of `docs/design/proposals/post-m35-directions.md` is the
big one — three sub-phases, ~600-900 LOC total, lifts every M35
single-hart simplification. Phase 1.0 is data-structure widening
only: no observable behavior change, no shepherd-task spawn API yet,
no preemption between user tasks. The slot widens; the lifecycle is
explicit; future phases ride on top.

1. **The slot is now a `Vec`.** The single
   `static mut ACTIVE: Option<ActiveUserTask>` has been replaced with
   `static mut USER_TASKS: Vec<ActiveUserTask>` plus
   `static mut CURRENT_USER_IDX: Option<usize>`. `active()` /
   `active_mut()` keep their `Option<&ActiveUserTask>` /
   `Option<&mut ActiveUserTask>` signatures and resolve via
   `USER_TASKS.get(CURRENT_USER_IDX?)`. Every syscall handler and
   the fault handler are unchanged — they still call
   `active()` / `active_mut()`.

2. **Push / pop is explicit.** Two new helpers replace the implicit
   `ACTIVE = Some(t)` / `ACTIVE = None` pattern:
   ```
   fn push_user_task(t: ActiveUserTask) -> usize;  // returns idx
   fn pop_user_task(idx: usize);
   ```
   `run_user_task_from_code_node` and `run_user_task_inner` each call
   `push_user_task` after building the address space and
   `pop_user_task` after the setjmp/longjmp return. The push/pop
   pair is balanced; the returned `idx` is owned by the caller for
   the duration of the U-mode run.

3. **Phase 1.0 preserves the single-active-task invariant.** Today
   `USER_TASKS` holds 0 or 1 entries at any moment. The kernel still
   runs only one user task at a time, and the shell's `cmd_spawn`
   call path is unchanged (synchronous `run_user_task_*` call).
   Phase 1.5 (shepherd-task spawn API) and phase 2 (timer-driven
   preemption + U-mode register save/restore in `ActiveUserTask`)
   will allow multiple entries to be alive simultaneously.

4. **Index stability.** `pop_user_task` removes the entry via
   `Vec::remove(idx)`, which is fine while `USER_TASKS.len() <= 1`.
   Phase 2 will need stable indices (so two concurrent tasks don't
   shift each other's `idx`), at which point this becomes either a
   slot-marker pattern (`Vec<Option<ActiveUserTask>>`) or a
   monotonic-key map. The proposal text picks `Vec<ActiveUserTask>`;
   phase 2 will refine.

5. **Verified end-to-end.** Smoke run exercises `userdemo` (M29
   read-cap + intentional violation), `editor` (M30 read+write +
   cascade-tombstone on exit), `ls 1` (M32 list_edges +
   SYS_MAP_NODE for the helios-std slab allocator), and `munmap`
   (post-M35 Proposal B — full SYS_UNMAP_NODE + slot reclaim
   round-trip). All four pass cleanly with the new
   push/pop lifecycle.

6. **What didn't change.** Cap caches still live in
   `ActiveUserTask`. `SYS_REVOKE_EDGE`, `SYS_DELEGATE_EDGE`,
   `SYS_MAP_NODE`, `SYS_UNMAP_NODE`, the cascade walk, the
   per-task `mappings` table — all unchanged. Phase 1.0 is purely
   the slot-shape migration; the syscall surface is byte-identical
   to M35 + Proposal B.

---

*Last reviewed: 2026-05-23 (post-M35 Proposal A phase 1.0 shipped — slot widened to a `Vec` + explicit push/pop lifecycle; phase 1.5 shepherd-task spawn API + phase 2 timer-driven U-mode preemption + phase 3 cross-task cap-cache and PT cleanup still pending). Proposal C shared-memory IPC continues to wait on full M36 (phases 1.5/2/3). Next review when phase 1.5 or material new cap-model work lands.*
