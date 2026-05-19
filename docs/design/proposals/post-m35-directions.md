# Post-M35 Directions

*Status: proposal, partially shipped. Written 2026-05-18 after M35
(CDT + delegation) shipped on 2026-05-10/11/12 and the closeout doc
landed on 2026-05-12 (`docs/design/capability-edges.md` "M35
Implementation Notes"). The previous proposal doc
(`post-m32-directions.md`) has been fully consumed — proposals
A/B/C shipped as M33/M34/M35. This is its successor.*

*Proposal B (`SYS_UNMAP_NODE`) shipped 2026-05-19. See the "Post-M35
Implementation Notes" section in `docs/design/capability-edges.md`
and the UART transcript at `screenshots/post-m35-munmap-uart.txt`.
Proposals A (multi-task scheduler M36) and C (shared-memory IPC)
remain open.*

## Context

M35 leaned on a "single active user task" assumption at three
explicit points (capability-edges.md M35 Implementation Note #9):

- **(a) cap-cache mutation only touches the active task** —
  `SYS_REVOKE_EDGE` rebuilds `read_allowed` / `write_allowed` /
  `traverse_allowed` / `grant_allowed` from scratch by re-walking
  the active task's `iter_live`. Fine when there is exactly one
  active user task; there is no question which cache to invalidate.
- **(b) page-table invalidation only happens on the active task** —
  the active task's `mappings: Vec<Mapping>` drives the
  `SYS_REVOKE_EDGE` unmap. A revoke from task A that tombstones an
  edge held by task B can't reach B's page table because B isn't
  running and the kernel has no `mappings` for it.
- **(c) cross-task cascade-during-run is structurally impossible** —
  the CDT tombstone walk only finds edges on the *current* task.
  Edges on a *different* task that descend from the revoked source
  stay live in B's page table until B exits.

These are clean simplifications, not bugs. They paid the
careful-design tax up front so the M35 typing was mechanical. They
also draw a hard line: anything that needs >1 user task running
concurrently needs them lifted.

Three things plausibly need that line lifted, plus several smaller
items deferred from M35. This doc enumerates the gap surface and
sketches one proposal per gap, leaving the decision to the author.

## The Three Open Gaps (in rough priority order)

### Gap 1: Single-slot user-task scheduler

`src/user.rs` keeps `static mut ACTIVE: Option<ActiveUserTask>` — a
single global slot. `run_user_task_with_caps` builds the address
space, drops to U-mode via `user_setjmp` + `enter_user`, and the
kernel "returns" via `user_longjmp` only on exit or fault. The
shell's `cmd_spawn` is synchronous; nothing else can run user code
in the meantime.

Meanwhile, the kernel-side scheduler (`src/task/mod.rs`) already
supports many concurrent kernel-mode tasks with cooperative
`yield_now` and preemptive `preemptive_yield` driven by the
supervisor timer interrupt. **Helios has multitasking; it just
doesn't have multi-user-tasking.**

This is the thesis-shaping next step. Multi-user-tasking is where
"everything is a memory" gets to mean something for live behavior,
not just storage. It's also the precondition for any real IPC,
service pattern, or shell-while-task-runs interaction.

**The fix:** widen the user-task slot to a table and lift each of
the M35 single-hart simplifications. Details in
[Proposal A](#proposal-a-multi-user-task-scheduler-m36).

### Gap 2: No `SYS_UNMAP_NODE` / per-allocation memory free

A user task can request a page-backed `NodeType::Memory` region via
`SYS_MAP_NODE`, but there's no way to release it before exit. The
M33 implementation note says: "the kernel has all the information
it needs (node id → L0 entries via the frame PA) — but it's not in
M33 because (a) there's no demo that needs it, and (b) the cleanest
API probably takes the `NodeId`, not the VA, which requires
plumbing a lookup that doesn't exist yet."

Two things changed since M33. M35 added a per-task
`mappings: Vec<Mapping>` that already carries each mapping's
`node_id`, so the NodeId → VA + L0-entry lookup is now O(mappings).
And M35's per-task tombstoning gave the kernel a clean pattern for
"task A wants its own state mutated under capability-check": pass
through `SYS_REVOKE_EDGE`-shaped plumbing.

`SYS_UNMAP_NODE` is small, obvious, and unblocks two things: (i)
long-running tasks that allocate/free in a loop, and (ii) any
future GC- or arena-style allocator built on top of helios-std.

**The fix:** add `SYS_UNMAP_NODE(node_id)`. Details in
[Proposal B](#proposal-b-sys_unmap_node).

### Gap 3: No shared-memory IPC primitive

M35 enabled cap delegation between tasks, but nothing in the
codebase actually delegates across two live user tasks because of
Gap 1. The most natural first IPC primitive is shared memory: task
A maps a Memory node, delegates `read` (and optionally `write`) to
task B, both tasks read/write the same backing frames. CDT handles
teardown: when A revokes (or exits), B loses access without B
having to know A is gone.

This is structurally tiny — M35 already shipped the delegation
syscalls, M33 already shipped Memory nodes, M35 already cascades
tombstones on task exit. What's missing is just (a) the second user
task to delegate to, and (b) a tiny user-space crate that
demonstrates round-tripping a message.

**The fix:** depends entirely on Gap 1 shipping first. Details in
[Proposal C](#proposal-c-first-shared-memory-ipc-demo).

## Three Proposals

### Proposal A: Multi-user-task scheduler (M36)

The big one. Three sub-options on cooperation strategy, two
sub-options on slot management. Picking the conservative shape in
each gives a small first version.

**Slot management — option A.1 (recommended):** widen
`ACTIVE: Option<ActiveUserTask>` to `USER_TASKS: Vec<ActiveUserTask>`
+ `current_user_idx: Option<usize>`. Keep `ActiveUserTask` as-is —
all the per-task state already lives there (page table, mappings,
cap caches, kctx, mem_node_ids). The kernel scheduler from
`src/task/mod.rs` continues to schedule kernel-mode tasks; user
tasks each get a *kernel-mode shepherd task* that owns one
`ActiveUserTask` slot and calls `run_user_task_inner`. When the
shepherd's user task yields (via timer or syscall), the shepherd
yields via `yield_now()` and the kernel scheduler picks the next
kernel-or-user task. This reuses the existing scheduler with
zero new scheduling code.

**Slot management — option A.2:** build a separate user-task
scheduler distinct from the kernel one. Rejected for first version:
two schedulers means two policies, two preempt counts, two ways for
state to drift. The shepherd-per-user-task pattern keeps one
scheduler.

**Cooperation strategy — option A.cosched.1 (recommended):**
**timer-driven preemption between user tasks**, via the existing
supervisor timer interrupt. When the timer fires inside U-mode (the
`from_umode` arm of `handle_timer_interrupt`), the kernel saves
the user task's full register state into its `ActiveUserTask`,
restores the kernel shepherd's state via `user_longjmp(.., -2)` (a
new sentinel meaning "preempted, not exited"), and the shepherd
calls `yield_now()`. Reuses M35's setjmp/longjmp plumbing as
implementation primitive — the shepherd's `kctx` is exactly the
right thing to longjmp to.

**Cooperation strategy — option A.cosched.2:** add a `SYS_YIELD`
syscall and require user tasks to cooperate. Rejected for first
version: makes user code's correctness depend on its own cooperation,
which is a weaker model than preemption and inconsistent with the
kernel-side scheduler that already preempts.

**Cooperation strategy — option A.cosched.3:** hybrid — preempt on
timer, also allow voluntary yield via `SYS_YIELD`. Reasonable second
move. Punt for first version: the M36 minimum is preemption alone.

#### What lifts each M35 simplification

**(a) cap-cache mutation across tasks.** When `SYS_REVOKE_EDGE` on
task A tombstones an edge that target-or-source touches task B's
cap caches, the kernel walks `USER_TASKS` and rebuilds the cap
caches for any task whose node references include either endpoint.
Cost: O(N_tasks * edges_per_task) per revoke. Mitigation: a small
reverse index from `target_node_id` → list of `(task_idx, label)`
that hold an edge to it. Build lazily; invalidate on cap-cache
rebuild. Not required for M36 first version — the linear walk is
fine at the scale of "tens of user tasks."

**(b) page-table invalidation across tasks.** When the cascade
tombstones an edge held by task B, the kernel walks B's `mappings`
and zaps the corresponding L0 PTEs. Hard part: TLB invalidation
when B isn't the current SATP. Two options. Option (i): `sfence.vma
zero, zero` only when switching *to* B (since B's TLB entries can't
be live in any other context). Option (ii): use ASIDs — give each
user task an ASID, issue `sfence.vma vaddr, asid` directly. Option
(i) is simpler and matches the M35 "flush everything on cap change"
philosophy; ASIDs are a profiling win, not a correctness one. Pick
(i) for M36.

**(c) cross-task cascade-during-run.** Falls out of (a) + (b): now
that revoke can reach across tasks, the cascade walks the whole
CDT and visits any task whose caps include each tombstoned edge.
The cascade itself is unchanged; the post-cascade fixup
(cap-cache + PT) is what lifted.

#### Suggested scope cuts for M36 first version

- **No SMP.** Stay single-hart. The kernel-side scheduler is
  already single-hart, and going SMP introduces a TLB-shootdown
  problem we don't need to solve while still proving the table
  works. M36 = "concurrent user tasks on one hart." A future M37+
  can do SMP if needed.
- **No `SYS_KILL`.** Tasks can still exit themselves via
  `SYS_EXIT`. Killing another task is a capability story
  (`grant`-style; "kill" cap?) that should ride on Gap 3's IPC
  work or later, not the first multi-user-task ship.
- **No priority scheduling.** Round-robin via `yield_now`,
  same as kernel-side today. Priorities are a tuning question.
- **No `wait` / parent-child reaping.** Shell's `cmd_spawn` becomes
  asynchronous, but the shell doesn't wait — it just queues the
  task and returns to the prompt. Exit codes become readable via
  the task graph node's content rather than via `cmd_spawn`'s
  return. Re-add a synchronous `spawn-wait` builtin if needed.
- **No fork / vfork.** New user tasks start from a code node like
  today; no per-task state inherited from a parent. Helios has
  never had fork; M36 doesn't add it.

#### Litmus binaries (analogous to M35's `cdtsmoke-α/β`)

- **`coexist-α`** — spawn two long-running counter tasks that print
  to UART. Verify both run by observing interleaved output (or
  alternating `preempt_count` increments on the task graph nodes).
- **`coexist-β`** — spawn task A that holds a `read` cap on a
  shared `Memory` node, then spawn task B that holds `traverse`
  but not `read` on the same node. A reads OK, B's read syscall
  returns `EPERM`. (This is the smallest test of "cap caches are
  per-task and don't bleed.")
- **`cdtsmoke-γ`** (the one M35 punted on) — A delegates `write` to
  B (now possible because B is a concurrent user task, not just a
  notion); A revokes; verify B's next write returns `EPERM` *and*
  B's PT mapping was zapped (the `wfi` after the revoke should see
  the unmap; B's first re-entry after revoke should fault).

Estimated implementation surface: ~600-900 LOC of kernel work plus
~150 LOC across the three litmus binaries. Three phases, mirroring
M35: (1) widen the slot + shepherd-task plumbing, (2) timer-driven
preemption + register save/restore, (3) lift the three M35
simplifications + litmus binaries. Each phase shippable
independently if needed.

### Proposal B: `SYS_UNMAP_NODE` *(shipped 2026-05-19)*

Symmetric to `SYS_MAP_NODE`. Takes a `node_id`, validates it's in
the calling task's `mem_node_ids`, walks the task's `mappings` for
that node, zaps the L0 PTEs, removes the graph node, flushes TLB.

**Cap gate:** the task already has `write` on its own Memory nodes
(self-granted at allocation time); `SYS_UNMAP_NODE` gates on
"node_id ∈ self.mem_node_ids" rather than introducing a new label.
The author shouldn't need a `destroy` cap to free their own
allocations; the cap-check shape that matches anonymous-`munmap`
on Unix is "did you allocate it?"

**Frame reclaim:** matches the M33 limitation — the Memory node
disappears but the backing frames don't return to a global free
pool (there is no global free pool yet). That's a separate
milestone (frame-level allocator); the syscall lands without it
and benefits the moment it lands.

**ABI (as shipped):**
```
SYS_UNMAP_NODE = 12  (M35's SYS_DELEGATE/REVOKE took 10/11; this is the next free slot)
Args:    a0 = node_id (u64)
Returns: a0 = 0       on success
                ENOENT (-2)  if node_id not in caller's mem_node_ids
                             (covers both "never allocated" and
                             "already freed"). The two distinguishable
                             failure cases collapse into one because a
                             U-mode syscall has an active task by
                             definition.
```

(The initial proposal sketch had `SYS_UNMAP_NODE = 9` and Linux-style
ENOMEM=-12; both were drafting errors. The shipped numbers match the
existing kernel-side errno constants and the post-M35 ABI cursor.)

**Demo:** `crates/munmap-user/` — allocates two regions, frees the
first, verifies via `list_edges(self)` that one Memory-edge
disappeared and the other remained.

Estimated surface: ~80 LOC kernel + ~50 LOC helios-std + ~30 LOC
demo. One-session-sized ship. Can land before M36 (no dependency)
or after (no conflict). Recommend landing **before** M36 — it's a
clean win, validates the M33 framework, and gives multi-task demos
a way to allocate-and-free in a loop without leaking.

### Proposal C: First shared-memory IPC demo

Depends on M36 shipping. The actual ABI surface is tiny because
M35 already shipped delegation:

1. Task A spawns. A's exec edges include `read+write` on a
   `Memory` node A allocated via `SYS_MAP_NODE`.
2. A spawns task B (via shell `cmd_spawn`, now asynchronous).
3. A calls `SYS_DELEGATE_EDGE(B, "read", mem_node_id)` — delegates
   read access. (Future: `write` for full shared-mem.)
4. B reads from the mem_node's mapped VA range. Sees what A wrote.
5. A revokes. B's next read EPERMs (cap-cache rebuilt) and pages
   come down (PT zapped). M36's cross-task cascade-during-run did
   the work.

**The one new piece:** A needs a way to *find out* B's node id to
delegate to it. Two options.

- **C.1 (recommended): shell-mediated** — `cmd_spawn` (asynchronous
  per M36) prints the new task's graph-node id to UART. The shell
  can pre-wire the relationship: `spawn ipc-producer 42; spawn
  ipc-consumer 43 42` where 42 is the shared mem node and 43 is
  the producer's task node. The user composes the topology.
- **C.2: rendezvous via known node** — A and B both look up a
  well-known node (`/tasks/ipc-rendezvous`), A writes its task id
  there, B reads it. Requires a shared write-cap on the rendezvous
  node, which is the same chicken-and-egg as the IPC problem
  itself, just one level out.

C.1 is the cleaner first move. C.2 belongs in a "service discovery"
milestone later.

**Demo:** `crates/ipc-producer-user/` writes "hello from A" to a
mem node; `crates/ipc-consumer-user/` reads it back. Both share
the producer's mem node via M35 delegation. Litmus binary
`cdtsmoke-γ` already verifies the revoke cascade — this is the
positive-path version.

Estimated surface: ~150 LOC across two crates. Trivial *if* M36
ships clean. The point isn't the LOC; the point is proving "two
user tasks share state through the graph" works end-to-end.

## Secondary Candidates (smaller, less load-bearing)

Things mentioned in passing in M33/M34/M35 notes that are worth
listing but don't merit their own proposal:

- **`SYS_LIST_EDGES_DETAIL` / `SYS_LIST_EDGES_V2`** — return each
  edge's `EdgeId` alongside its label byte, for callers that want
  to drive `SYS_REVOKE_EDGE` by id rather than by `(target, label)`.
  Punted in M35 because the kernel-side resolve is O(edges-on-self)
  and cheap. Revisit if/when an interactive grant editor wants
  id-stability across `list_edges` calls.
- **`map_node` delegation policy / `grant` cap on allocation** —
  M35 `map_node` self-grants `write` but not `grant`, so a task
  can't onward-delegate its anonymous memory without an explicit
  grant edge. The grant-policy note (`knowledge/notes/cdt-grant-
  policy.md`) captured the per-target-only-during-spawn decision;
  the auto-grant-on-map_node option (b) stays separable. Revisit
  if/when a real workload wants A→B→C delegation chains on heap.
- **`helios-libc`** — gate to `busybox`, `vim`, `lua` ports. Still
  not blocking; the thesis is graph-native first. Re-evaluate
  after Proposal C lands, when the next concrete need is probably
  "an existing program, but talking to the graph."
- **Host-side graph test harness** — kernel `Graph` primitives
  can't be `cargo test`-ed today (kernel is `no_std`). M35 notes
  registered this as possible future work — a thin "graph-host-
  test" crate that wraps the pure-data graph operations. Useful
  for catching regressions before they hit QEMU. Niche; do when
  a regression actually slips through.

## Recommendation

**Order: B → A → C.** (B shipped 2026-05-19. A and C remain.)

`SYS_UNMAP_NODE` (Proposal B) first. It's a clean, well-bounded
win, removes a real M33 limitation, and is the smallest piece of
M35 vocabulary I haven't exercised. Approximately one session.
*Result: shipped in one session as expected; ~120 LOC of kernel
work + ~290 LOC of demo + helios-std wrappers + docs. See
`screenshots/post-m35-munmap-uart.txt` for the end-to-end UART
transcript.*

Then M36 (Proposal A). The big one. Three phases over multiple
sessions, mirroring M35's shape:
1. Slot widening + shepherd-task plumbing + asynchronous
   `cmd_spawn`.
2. Timer-driven preemption + full register save/restore + ASID-free
   TLB flush on switch.
3. Lift M35's three single-hart simplifications + the three
   litmus binaries (`coexist-α`, `coexist-β`, `cdtsmoke-γ`).

Then IPC (Proposal C). Falls out cheap once M36 is in.

This ordering matches the post-M32 doc's argument that
"smaller-prerequisite-of-bigger" goes first. It also lets Proposal
B validate the M33 framework before M36 builds on M33+M35 together.

## Open Questions for Author

These are the decisions I don't want to make unilaterally — same
shape as the post-M32 doc's open questions.

1. **Preemption strategy for user tasks: timer-only, cooperative
   only, or hybrid?** Recommended A.cosched.1 (timer-only) above.
   But the kernel scheduler is hybrid (cooperative `yield_now` +
   preemptive timer); consistency-with-kernel would argue for
   hybrid in user-mode too via `SYS_YIELD`. Cost is one extra
   syscall number and a tiny dispatch; benefit is letting user
   code that knows it's idle be a good citizen.

2. **What signals user-task exit to a future `wait`-er?** M36
   first version ditches `wait` (the task graph node's content
   carries the exit code). But if/when a parent wants to wait on
   a child, two shapes: (a) a `SYS_WAIT(task_node_id)` that
   blocks on the task's `state` going to Done, (b) a `Future`-on-
   graph-edge pattern where the parent reads an edge that resolves
   on child exit. (b) is more graph-native; (a) is more familiar.
   Punt for M36; flag the decision.

3. **Should `cmd_spawn` print the new task node id, or just store
   it in the graph?** Proposal C.1 assumes printed. If we want
   spawn to be silent (and recoverable via `ls /tasks`), C.2 (or
   a third "spawn-returns-id-via-side-channel" pattern) is needed.
   Small decision but it shapes shell ergonomics. Recommend
   printed for M36, revisit when an actual workflow complains.

4. **ASIDs now or later?** TLB management gets meaningfully better
   with ASIDs (selective flush vs. full flush on every switch).
   But the kernel-side scheduler doesn't use them, and the M35
   PT-invalidation logic was happy with full flush. The cost-
   benefit depends on context-switch frequency. Recommend "no
   ASIDs for M36; revisit when profiling shows TLB pressure."

5. **Does Proposal C's IPC demo want one-way or two-way sharing?**
   Two-way (both A and B have `write`) is the more general case
   but raises ordering / lock questions immediately. Recommend
   one-way for the *first* demo (A produces, B consumes, no
   feedback channel), then expand. Lockless ringbuffer or
   message-queue patterns belong to whatever comes after C.

---

*This doc is a proposal, not a decision. It enumerates the gap
surface visible after M35 and sketches one possible shape for each
gap. The author may take any of these, none of them, or a different
ordering. Successor doc to `post-m32-directions.md`.*

*Last reviewed: 2026-05-18 (post-M35 reflection — six days after
the M35 closeout doc landed on 2026-05-12). Re-review when any of
A/B/C ships, or when material new information about the gap surface
appears.*
