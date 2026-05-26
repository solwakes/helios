# M36 Phase 2: Timer-driven U-mode preemption

*Status: design draft, 2026-05-26. Successor to `post-m35-directions.md`
Proposal A phase 2 sketch. Phase 1.0 (slot widening) shipped 2026-05-23
in commit `11290ea`. Phase 1.5 plumbing shipped 2026-05-24 (`f31f5dd`),
integration shipped 2026-05-25 (`e02bac3`). This doc enumerates the
phase 2 implementation surface so the next code-channel session can
ship without doing new design under time pressure.*

## Context: where phase 1.5 left us

Today every modern-path user task runs on a dedicated kernel shepherd
task with its own 16 KiB stack:

```
shell task
  └── cmd_spawn argecho 42
        └── run_user_task_via_shepherd(...)
              ├── spawn_user_shepherd(...)         spawn fresh kernel task
              │     ↑ stashes (task_node_id, arg0, arg1) in SHEPHERD_SLOT
              ├── task::wait_for_task(shepherd_id) cooperative join
              │     ↑ yields shell-task while shepherd runs
              └── collect_user_shepherd_result()   reads slot.result
shepherd task (fresh kernel task, own 16 KiB stack)
  └── shepherd_entry(_arg)
        ├── reads SHEPHERD_SLOT
        ├── run_user_task_inner(task_node_id, arg0, arg1)
        │     ├── build address space
        │     ├── push_user_task(ActiveUserTask{...})
        │     ├── user_setjmp(&kctx)                ← jmp = 0 first time
        │     ├── enter_usermode_asm(...)           drops to U-mode
        │     │     ↓ U-mode runs until ecall/exception/SYS_EXIT
        │     │     ↓ trap entry → trap_handler → handle_syscall / handle_user_fault
        │     │     ↓ user_longjmp(kctx, 1)
        │     └── ← here (jmp = 1): collect exit_code, pop task, cascade
        ├── stashes exit_code in SHEPHERD_SLOT.result
        └── returns (shepherd task transitions to Done)
```

The key feature: **the shepherd is the only thing running in S-mode while
the user task is in U-mode**. When U-mode yields the hart (via syscall
or fault), control returns to the shepherd's S-mode context via
`user_longjmp`. The shepherd's `run_user_task_inner` returns normally,
the shepherd writes the result, the shepherd exits.

The constraint phase 1.5 still enforces: `wait_for_task` blocks the
caller, so only one shepherd is staged-and-live at a time. That's what
makes the singleton `SHEPHERD_SLOT` safe. *Phase 2 lifts both of these
together.*

## What phase 2 changes

**Goal:** the supervisor timer interrupt can fire while a user task is
running, and the kernel switches to a *different* user task (or kernel
task) without the running user task voluntarily yielding.

This requires four pieces of work, in roughly increasing complexity:

1. **Per-task U-mode register state.** Add `saved_frame: Option<TrapFrame>`
   to `ActiveUserTask` so the kernel can stash a preempted task's full
   U-mode register state across a context switch.

2. **Trap-handler timer-from-U-mode branch.** When the supervisor timer
   fires inside U-mode, copy the on-stack `TrapFrame` into the current
   `ActiveUserTask`'s `saved_frame`, then `user_longjmp` back to the
   shepherd's `kctx` with a new sentinel value (e.g. `PREEMPT = 2`).

3. **A resume path back to U-mode.** New assembly routine
   `resume_user_frame(frame_ptr) -> !` that restores the saved register
   state and `sret`s back into U-mode — symmetric to `enter_usermode_asm`,
   but using a saved frame instead of a fresh entry.

4. **Multi-shepherd safety: the slot indirection.** `SHEPHERD_SLOT`
   becomes a per-shepherd-id map, and `cmd_spawn` stops blocking on
   `wait_for_task`. After this, multiple user tasks can be live
   simultaneously.

The first three are the load-bearing kernel changes. The fourth is
small but exposes the previous three to the actual concurrent case.

## Decision points (resolve before implementing)

These are choices the implementing session will need to make. I'm
recording the recommended answer + the alternative + the trade-off so
the decision is fast in-session.

### D1: How does the shepherd resume after preemption?

**Recommended: loop inside `run_user_task_inner`.** The shepherd's
top-level call shape stays the same. Inside `run_user_task_inner`,
wrap the `setjmp`/`enter_usermode` pair in a loop that re-`setjmp`s on
preempt and either calls `enter_usermode_asm` (first iteration) or
`resume_user_frame(&saved_frame)` (subsequent iterations after a
preempt-yield-resume cycle).

```rust
loop {
    let jmp = unsafe { user_setjmp(kctx_ptr) };
    if jmp == 0 {
        // setjmp installed; drop to U-mode for the first time, OR
        // resume from a saved frame.
        match active().and_then(|a| a.saved_frame.as_ref()) {
            None => enter_usermode_asm(...),       // first entry
            Some(frame) => resume_user_frame(frame as *const _),
        }
    } else if jmp == JMP_PREEMPT {
        // Frame was copied into active.saved_frame by trap_handler.
        crate::task::yield_now();
        continue;  // loop: setjmp again, then resume.
    } else {
        // jmp == JMP_EXIT_OR_FAULT (current value 1). Break out.
        break;
    }
}
```

The `active().saved_frame.is_some()` check distinguishes "first entry"
from "resume" inside the `jmp == 0` arm. This keeps the resume path
out of `enter_usermode_asm` (which still does its fresh-entry zeroing
of caller-saved regs).

**Alternative: shepherd-level loop.** Have `run_user_task_inner`
return on preempt, shepherd yields then re-enters. Rejected: forces
`run_user_task_inner` to expose its address-space setup as
"resumable," which leaks more internal state than the in-function loop.

### D2: Where does the preempted frame live?

**Recommended: `saved_frame: Option<TrapFrame>` field on
`ActiveUserTask`.** The frame is ~272 bytes; storing it inline in the
task's owned state is the simplest path. Phase 2 promotes it from
"data on the trapping shepherd's stack" (where it gets reclaimed when
the shepherd's `setjmp` rolls sp back) to "data the shepherd owns
across the yield."

The trap handler memcpys the frame from kernel stack to
`active_mut().saved_frame = Some(*frame.clone())` before longjmping.

**Alternative: a kernel allocator slab indexed by task id.** Rejected
for first version. The inline field is the smallest mutation to the
existing struct and trivially correct.

### D3: How does the trap handler signal preempt-vs-exit-vs-fault?

**Recommended: a new longjmp sentinel value, plus `saved_frame`
presence.**

```rust
const JMP_EXIT_OR_FAULT: usize = 1;  // existing, unchanged
const JMP_PREEMPT: usize = 2;        // new
```

`handle_timer_interrupt`, when called with `from_umode = true` and an
active user task, replaces today's "return to U-mode" path with:

```rust
fn handle_timer_interrupt(frame: &mut TrapFrame, from_umode: bool) {
    // ...existing counter / re-arm code...
    if from_umode {
        // Copy the U-mode register state into the current task.
        if let Some(a) = active_mut() {
            a.saved_frame = Some(unsafe { core::ptr::read(frame as *const _) });
            let ctx = a.kctx;
            // Longjmp back to the shepherd; trap-epilogue restore is
            // skipped because we never return from handle_timer_interrupt.
            unsafe { user_longjmp(ctx, JMP_PREEMPT); }
        }
        // No active user task — fall through to return to U-mode (which
        // is impossible: if from_umode then there's an active task).
        // The branch is here for type safety only.
        return;
    }
    crate::task::preemptive_yield();
}
```

Note the signature change: `handle_timer_interrupt` now needs the
TrapFrame pointer so the U-mode register state is visible. This is a
one-call-site change in `trap_handler`.

The fault path (`handle_user_fault`) and exit path (`sys_exit`) still
use `JMP_EXIT_OR_FAULT = 1` — they're unchanged.

**Alternative: a single longjmp sentinel + an `is_preempted` flag.**
Rejected: two pieces of state where one would do. The sentinel is
cheaper.

### D4: How does `resume_user_frame` work?

It's a near-clone of `enter_usermode_asm`'s tail, plus the trap
epilogue's restore-from-frame logic. Three constraints determine
the instruction ordering:

1. **The frame pointer arg lives in a0 (x10).** Once we restore `x10`
   from the frame, the frame pointer is lost. So `x10` must be the
   last (or second-to-last) restore.
2. **`sscratch` must hold the kernel sp the next U-mode trap should
   land on** — *our current kernel sp at the moment of sret*. Once
   we restore `x2` (sp) from the frame, sp = user sp; the kernel sp
   is gone. So `csrw sscratch, sp` must happen *before* restoring
   x2 (and before restoring x10, since x10 is needed to address the
   frame).
3. **SATP must be the user task's SATP when sret fires.** Either the
   caller is responsible (`csrw satp, aspace.satp; sfence.vma 0, 0`
   before invoking the routine), or the routine takes satp as a1.
   The latter mirrors `enter_usermode_asm`'s a0=satp pattern;
   consistency favors it.

Sketch (a1 = satp; a0 = frame pointer):

```
.globl resume_user_frame
resume_user_frame:
    # 1. Switch SATP.
    csrw  satp, a1
    sfence.vma zero, zero

    # 2. Restore sepc.
    ld    t0, 32*8(a0)
    csrw  sepc, t0

    # 3. Configure sstatus.SPP=0 (return to U), SPIE=0, SUM=1.
    csrr  t1, sstatus
    li    t2, 0x100
    not   t2, t2
    and   t1, t1, t2
    li    t2, 0x20
    not   t2, t2
    and   t1, t1, t2
    li    t2, 0x40000
    or    t1, t1, t2
    csrw  sstatus, t1

    # 4. Save current kernel sp into sscratch BEFORE restoring x2.
    csrw  sscratch, sp

    # 5. Restore all GPRs except x2 (sp) and x10 (a0=frame ptr).
    ld    x1,  1*8(a0)
    ld    x3,  3*8(a0)
    ld    x4,  4*8(a0)
    # ... (x5..x9, x11..x31)
    ld    x31, 31*8(a0)

    # 6. Restore x10 (a0) and x2 (sp) last; sret.
    ld    x10, 10*8(a0)
    ld    x2,  2*8(a0)
    sret
```

**Final signature: `resume_user_frame(frame: *const TrapFrame, satp:
usize) -> !`.**

### D5: How does cmd_spawn become asynchronous?

This is the *behavioral* lift of phase 2 — phases 1-4 above are
"plumbing for when N>1 user tasks are live at once," and this is the
piece that actually produces N>1.

**Recommended for first-version M36: keep `cmd_spawn` synchronous via
`spawn-wait` syntax; add `spawn-async` for non-blocking.** I.e.,
preserve the existing shell command's blocking behavior, but add a
sibling command that returns immediately. This avoids breaking demos
that count on `cmd_spawn` blocking (every test transcript in
`screenshots/` since M29).

```
helios> spawn argecho 42       # blocks, prints result (today's behavior)
helios> spawn-async hello      # returns immediately, prints shepherd id
[shell] task #N spawned, shepherd #M
helios> ps                     # see hello running
helios> ls /tasks/N            # find its exit_code edge once done
```

`spawn-async` invokes `spawn_user_shepherd(...)` and *returns* without
calling `wait_for_task`. That immediately exposes the multi-shepherd
case: the next `spawn`/`spawn-async` from the shell can now create a
second live shepherd, and the timer's preemption logic gets exercised.

**Alternative: rename `spawn` to be async by default, add `spawn-wait`
for the old behavior.** Cleaner long-term but breaks every existing
demo. Punt to a later milestone.

### D6: How does the per-shepherd slot map work?

With `spawn-async` producing multiple live shepherds, the singleton
`SHEPHERD_SLOT` no longer fits.

**Recommended: pass slot ownership through `spawn_with_arg`'s
arg-usize.** Heap-allocate a `Box<ShepherdSlot>`, leak it, pass the
pointer as the `arg`. The shepherd's `_arg` parameter (currently
ignored) becomes the slot pointer. Shepherd reads inputs from the
slot, writes result back, then drops the box.

```rust
fn shepherd_entry(arg: usize) {
    let slot_ptr = arg as *mut ShepherdSlot;
    let (task_node_id, arg0, arg1) = unsafe {
        ((*slot_ptr).task_node_id, (*slot_ptr).arg0, (*slot_ptr).arg1)
    };
    let rc = run_user_task_inner(task_node_id, arg0, arg1);
    unsafe { (*slot_ptr).result = rc; }
    // Slot's lifetime is decided by the spawner: it may be polled,
    // or freed once the result is read. Today's pattern is one-shot
    // poll-and-free; phase 2.5 may want a longer-lived result edge.
}

pub fn spawn_user_shepherd(task_node_id: u64, arg0: usize, arg1: usize) -> SpawnHandle {
    let slot = Box::leak(Box::new(ShepherdSlot { task_node_id, arg0, arg1, result: -1 }));
    let id = crate::task::spawn_with_arg("user-shepherd", shepherd_entry, slot as *mut _ as usize);
    SpawnHandle { task_id: id, slot: slot as *mut _ }
}
```

`SpawnHandle` is the new owning handle. Drop / collect zeroes the slot.

`collect_user_shepherd_result(handle)` reads `handle.slot.result` and
drops the box.

**Alternative: a Vec-of-slots indexed by task_id.** Rejected — task
ids are globally unique kernel task ids, but using them as map keys
means a hashmap or O(n) scan, where the leaked-Box approach is O(1)
and the lifetime is exactly the right shape.

## Implementation surface estimate

Following the M35 sub-phase pattern (each ship-able in one session
once design is done):

| Sub-phase | Surface | What ships |
|---|---|---|
| 2.0 | ~50 LOC `src/user.rs` + ~5 LOC `src/trap.rs` | Add `saved_frame: Option<TrapFrame>` field; wire `JMP_PREEMPT` sentinel; trap_handler timer-from-U branch copies frame and longjmps; `run_user_task_inner` loop with `if saved_frame.is_some() { resume } else { enter }` distinction. *No new asm yet — first ship just verifies the longjmp path.* |
| 2.1 | ~60 LOC asm + ~20 LOC Rust | `resume_user_frame` asm routine. After 2.0, `enter_usermode_asm` is still called on every iteration of the loop; 2.1 switches the resume path to the new routine. Verify with a U-mode binary that doesn't yield voluntarily — should now make forward progress under timer preemption. |
| 2.2 | ~80 LOC Rust | `SHEPHERD_SLOT` becomes leaked-box per-shepherd. `spawn_user_shepherd` returns `SpawnHandle`. `collect_user_shepherd_result(handle)`. Update `run_user_task_via_shepherd` to use new API. Existing single-shepherd behavior unchanged because handle is short-lived. |
| 2.3 | ~30 LOC `src/shell.rs` + ~20 LOC `src/user.rs` | `cmd_spawn_async` shell builtin. Demonstrates N=2 user tasks via concurrent `spawn-async coexist-α; spawn-async coexist-α` from the shell. UART transcript shows interleaved output. |

Total: ~250 LOC + transcripts. Each sub-phase ships in its own session.

A **`coexist-α`** litmus binary should land alongside 2.3: a U-mode
loop that prints a digit every ~10ms (via `SYS_PRINT`) and exits
after N iterations. Two concurrent runs should produce interleaved
digits if preemption works. ~30 LOC of user-space code.

## What this proposal does NOT cover

- **Cross-task cap-cache invalidation.** When task A revokes an edge
  that B also holds (e.g., A delegated `read` on a node to B, then
  revoked), B's `read_allowed` Vec still has the target node id.
  M35's `SYS_REVOKE_EDGE` rebuilds the *current* task's cap caches
  only. After phase 2 produces N>1 live user tasks, this is a
  correctness gap. M35 Implementation Note #9 (in capability-edges.md)
  flagged this as the single-hart simplification (a).
- **Cross-task PT invalidation.** Same shape as above, for the PT
  zap. The page-table-invalidation logic walks `active().mappings`;
  with N>1 tasks, it needs to walk *every* affected task's mappings.
  M35 simplification (b).
- **Cross-task cascade-during-run.** Falls out of (a) + (b).
  M35 simplification (c).

These are *phase 3* work: lifting the three single-hart simplifications
+ litmus binaries `coexist-β` and `cdtsmoke-γ`. Estimated surface:
~200 LOC kernel + 60 LOC litmus binaries. Ships in its own session(s)
after phase 2 is solid.

## Risk-register

Each item lists the failure mode + the most likely diagnosis path.

- **The user task makes no progress under preemption.** Most likely:
  the resume path's register restore is wrong. Diagnose by adding a
  pre-`sret` UART print of `frame.sepc` and the first few regs, and
  comparing across iterations. Should see sepc advance over time.
- **The shepherd's kernel stack corrupts on resume.** Most likely:
  `sscratch` not set correctly, or set to a stale value, so the
  next U-mode trap lands on the wrong stack. Diagnose by reading
  `sscratch` at trap entry (already in `_trap_entry`'s swap logic;
  add a print).
- **Two concurrent user tasks observe each other's data.** Most likely:
  SATP not switched correctly on resume — task B starts running but
  with task A's PT live. Diagnose by reading `satp` at trap entry
  and at U-mode entry; compare to `aspace.satp`.
- **Timer interrupts fire while in the trap handler.** Already
  handled today (sstatus.SIE is cleared on trap entry by hardware
  and not re-enabled until sret). Should not regress, but worth
  asserting via a debug check that no nested timer fires during
  the U-mode → frame-copy → longjmp window.

## Out-of-scope items (still)

Per `post-m35-directions.md` "Suggested scope cuts for M36 first
version":

- No SMP. Single-hart only.
- No SYS_KILL. Tasks exit themselves.
- No priority scheduling. Round-robin via existing `yield_now`.
- No fork / vfork. Tasks start from code nodes.
- No ASIDs. Full TLB flush on switch (already the M35 pattern).

These remain unchanged for phase 2.

---

*Phase 2 design draft. Open questions resolved above with explicit
recommendations + alternatives. Implementation should be feasible in
~4 sessions following the sub-phase split. The bias toward "design
without commit" continues to be the load-bearing decision: phase 1.0
and phase 1.5 each shipped in one session because their design was
already done. This doc is the equivalent for phase 2.*

*Last reviewed: 2026-05-26 (after phase 1.5 integration shipped
2026-05-25). Re-review if any sub-phase ships or if material new
information about the gap surface appears.*
