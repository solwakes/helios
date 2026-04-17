# Capability Edges: Graph-Native Security

*Status: Design committed M28, first implementation M29. This document describes the model; implementation details follow as they land.*

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

## Syscall API (M29)

M29 starts with a minimal ABI:

| Num | Name          | Args                              | Returns                         |
|-----|---------------|-----------------------------------|---------------------------------|
| 1   | `READ_NODE`   | `a0`=node_id, `a1`=buf, `a2`=len  | bytes read, or -EPERM / -ENOENT |
| 2   | `PRINT`       | `a0`=buf, `a1`=len                | 0                               |
| 3   | `EXIT`        | `a0`=code                         | (no return)                     |

`READ_NODE` checks the task's outgoing edges for a `read` or `write` labeled edge to the target. If present: copy content. If absent: `-EPERM`.

Later milestones will add:
- `WRITE_NODE`, `APPEND_NODE` (gated on `write` edge)
- `FOLLOW_EDGE`, `LIST_EDGES` (gated on `traverse` edge)
- `CREATE_NODE`, `ADD_EDGE` (gated by a task's authority to create — probably another cap)
- `MAP_NODE` (map target into caller's address space for direct access, avoiding syscall overhead)

## Delegation and Revocation

When task A delegates a capability to task B, A is copying one of its outgoing edges to be outgoing from B. Example: `A → framebuffer [write]` plus `add_edge(B, framebuffer, write)` = `B → framebuffer [write]`.

For proper revocation semantics, we need to know that B's edge is *derived from* A's. Otherwise revoking A's access would leave B's edge orphaned but still valid.

The canonical solution is a **capability derivation tree (CDT)**: each derived edge knows its parent edge. Revoking a parent cascades to all descendants. This is how seL4 handles revocation.

In graph terms: delegated edges get a `derived_from` back-link to the source edge. Revoke source → walk the derivation tree → remove descendants.

CDT semantics are planned for a later milestone (M31+ probably). M29 ships without delegation — edges are kernel-declared-only.

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

- **M29**: Skeleton — one U-mode task, MMU enforcement, 3 syscalls. Cap violation = task kill.
- **M30**: Expand syscalls (`WRITE_NODE`, `FOLLOW_EDGE`, `LIST_EDGES`).
- **M31**: Cap delegation + CDT for revocation.
- **M32**: Multiple user tasks coexisting.
- **M33**: Port DOOM to user mode (the litmus test — does the cap model handle a big, real program?).

---

*Last reviewed: 2026-04-16 (post-M28). Next review after M29 implementation.*
