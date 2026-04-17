# Capability Edges: Graph-Native Security

*Status: Design committed M28, first implementation M29, ABI expanded M30 + M33 + M34. This document describes the model; implementation details follow as they land.*

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

## Syscall API (M29 + M30 + M33 + M34)

The ABI is append-only and numbered; higher numbers were added in later
milestones. M30 introduced the `traverse` capability kind; M33 added
`MAP_NODE` for kernel-granted anonymous writable memory; M34 added
`READ_EDGE_LABEL` to let user programs see structural edge labels
(`child`, `parent`, etc.) that `LIST_EDGES` only reports as `unknown`.

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

### Cap-check kinds

The syscall layer uses a `has_cap(target, label)` helper that scans the
current task's outgoing edges for an edge with the given label pointing at
`target`. Four labels are recognised today:

- `read`  — grants `READ_NODE` and is an R-only MMU mapping.
- `write` — grants `WRITE_NODE`, auto-implies `read`, and is an R/W MMU mapping.
- `exec`  — grants execution of code from the target, R/X MMU mapping.
- `traverse` — grants `LIST_EDGES` / `FOLLOW_EDGE` from the target; does
  NOT map anything into the task's page table (it's a pure-syscall cap).

### LIST_EDGES entry layout (16 bytes each, little-endian)

```
offset  type    meaning
 0      u64     target node id
 8      u8      label kind: 0=unknown, 1=read, 2=write, 3=exec, 4=traverse
 9      u8[7]   padding (zero)
```

Entries are returned in the order edges were added to the source node
(the graph's `Vec<Edge>` iteration order). `FOLLOW_EDGE` also respects
this order and returns the *first* matching edge.

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
- `CREATE_NODE`, `ADD_EDGE` (gated by a "create"/"grant" cap — target: M34+, same milestone as CDT)
- `UNMAP_NODE` / `map_node` free path — M33 has no per-allocation
  reclaim. A region dies with the task.
- Rerouting helios-std's `GlobalAlloc` through `MAP_NODE` — the syscall
  ships in M33, but the in-binary bump heap is still what backs
  `alloc::*`. Shrinking it and chaining kernel-granted slabs is a
  follow-on.

## Delegation and Revocation

When task A delegates a capability to task B, A is copying one of its outgoing edges to be outgoing from B. Example: `A → framebuffer [write]` plus `add_edge(B, framebuffer, write)` = `B → framebuffer [write]`.

For proper revocation semantics, we need to know that B's edge is *derived from* A's. Otherwise revoking A's access would leave B's edge orphaned but still valid.

The canonical solution is a **capability derivation tree (CDT)**: each derived edge knows its parent edge. Revoking a parent cascades to all descendants. This is how seL4 handles revocation.

In graph terms: delegated edges get a `derived_from` back-link to the source edge. Revoke source → walk the derivation tree → remove descendants.

CDT semantics are planned for M32 (originally scheduled for M31 before that slot was redirected to shipping helios-std). M29–M31 ship without delegation — edges are kernel-declared-only.

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
- **M33** (done): `SYS_MAP_NODE` — kernel-granted anonymous writable memory. Tasks can mint fresh `NodeType::Memory` nodes at runtime; the kernel allocates backing frames, adds a `write` edge from caller → new node (implying `read`), and maps the frames into the task's data-VA window. Demo at `crates/mmap-user/` (`spawn mmap`). The helios-std `GlobalAlloc` backend has *not* been rerouted through `map_node` yet — that's an additive refactor deferred so the milestone stays scoped to "unblock future cool stuff". See "M33 Implementation Notes" below.
- **M34** (done): `SYS_READ_EDGE_LABEL` — read a single outgoing edge's full UTF-8 label by index. Closes the "everything shows as `?`" gap: `SYS_LIST_EDGES` keeps its compact 16-byte entries with a cap-kind byte, and user code issues one follow-up syscall per structural edge it wants the actual label for. Shipped as append-only (ABI not broken); `spawn ls 1` now prints `child` for all 19 root outgoing edges instead of `?`. See "M34 Implementation Notes" below.
- **M35**: Cap delegation + CDT for revocation. (Was going to be M34; `SYS_READ_EDGE_LABEL` shipped first because it's a ~40 LOC kernel change and `ls` was staring at `?` for three milestones straight.)
- **M36**: Multiple user tasks coexisting.
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

3. **The bump allocator lives inside the binary.** Because there's no
   `SYS_MAP_NODE` / anonymous page grant yet, `helios-std`'s global
   allocator is backed by a 64 KiB static `[u8; N]` in the binary
   image itself. `static mut X: [u8; N] = [0; N]` would get placed in
   `.bss`, which `objcopy -O binary` drops; an explicit non-zero
   initializer (`[0xAA; N]`) forces it into `.data` so the kernel
   actually copies the page bytes. See `crates/helios-std/src/heap.rs`.

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

---

*Last reviewed: 2026-04-17 (post-M34 `SYS_READ_EDGE_LABEL`: `ls 1` prints full structural labels). Next review after the heap-allocator refactor or CDT lands.*
