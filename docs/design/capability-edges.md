# Capability Edges: Graph-Native Security

*Status: Design committed M28, first implementation M29, ABI expanded M30. This document describes the model; implementation details follow as they land.*

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

## Syscall API (M29 + M30)

The ABI is append-only and numbered; higher numbers were added in later
milestones. M30 also introduced the `traverse` capability kind.

| Num | Name           | Args                              | Cap check              | Returns                              |
|-----|----------------|-----------------------------------|------------------------|--------------------------------------|
| 1   | `READ_NODE`    | `a0`=node_id, `a1`=buf, `a2`=len  | `read` or `write` edge | bytes read, or -EPERM / -ENOENT      |
| 2   | `PRINT`        | `a0`=buf, `a1`=len                | — (bounds-checked)     | bytes printed                        |
| 3   | `EXIT`         | `a0`=code                         | —                      | (no return)                          |
| 4   | `WRITE_NODE`   | `a0`=node_id, `a1`=buf, `a2`=len  | `write` edge           | bytes written, or -EPERM / -ENOENT / -EINVAL |
| 5   | `LIST_EDGES`   | `a0`=src_id, `a1`=buf, `a2`=max   | `traverse` edge to src | #entries written, or -EPERM / -ENOENT |
| 6   | `FOLLOW_EDGE`  | `a0`=src_id, `a1`=label, `a2`=len | `traverse` edge to src | target_id, or -EPERM / -ENOENT       |
| 7   | `SELF`         | —                                 | — (always allowed)     | caller's task node id                |

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

### Copy-in / copy-out bounds

User-space buffers are verified to lie strictly within the user VA
window `[0x4000_0000, 0x4020_0000)` before any kernel read/write. SUM is
already set in `sstatus`, so once the bounds check passes the kernel
dereferences the pointer directly. Out-of-range buffers → `-EINVAL`.

### Still planned

- `APPEND_NODE` (gated on `write` edge)
- `CREATE_NODE`, `ADD_EDGE` (gated by a "create"/"grant" cap — target: M32+, same milestone as CDT)
- `MAP_NODE` (map target into caller's address space for direct access,
  avoiding syscall overhead). Also the intended replacement for
  helios-std's fixed-size bump allocator from M31 — once a task can
  request fresh zero pages at runtime, `alloc::*` can have a real
  heap instead of 64 KiB carried in the binary image.

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
- **M33**: Cap delegation + CDT for revocation. (Previously planned for M31 but the userspace library caught up to the design doc first.)
- **M34**: Multiple user tasks coexisting.
- **M35**: Port DOOM to user mode (the litmus test — does the cap model handle a big, real program?).

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

---

*Last reviewed: 2026-04-17 (post-M32 graph-native Rust tools: `spawn ls`, `spawn cat`). Next review after M33 CDT/delegation.*
