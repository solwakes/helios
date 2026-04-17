/// User-space support for Helios (M29).
///
/// This module is the core research contribution: **graph edges ARE
/// capabilities**. A user task's outgoing edges with labels `read`,
/// `write`, `exec` define the only nodes it can reach. We build a
/// per-task Sv39 page table that maps exactly those nodes (with MMU
/// permissions matching the edge label) plus the kernel regions needed
/// to service traps. Any access the task doesn't have an edge for -> no
/// mapping -> page fault -> kernel kills the task.
///
/// Syscalls are also checked against the same edge set: SYS_READ_NODE on
/// a node you don't have a `read`/`write` edge to returns -EPERM.

use core::arch::global_asm;
use core::sync::atomic::{AtomicUsize, Ordering};

use alloc::vec::Vec;

use crate::arch::riscv64 as arch;
use crate::graph::{self, NodeType};
use crate::mm::page_table::{
    alloc_page_table, alloc_user_frame, PageTable, PageTableEntry,
    PTE_R, PTE_U, PTE_W, PTE_X,
};
use crate::mm::SATP_MODE_SV39;
use crate::trap::TrapFrame;

// ---------------------------------------------------------------------------
// User virtual-address layout
// ---------------------------------------------------------------------------

/// VA base for `exec` edge content (code). First exec edge is the entry
/// point. Each edge gets its own 4 KiB page starting here.
pub const USER_CODE_BASE: usize = 0x4000_0000;

/// VA base for `read`/`write` edge content. Each such edge gets its own
/// 4 KiB page starting here. Exposing node content directly in user VA
/// means the task could consume it by MMU (read/write in its own VA)
/// as an alternative to syscall — M29 syscalls are what we demo, but the
/// mapping makes the capability _really_ enforced by hardware.
pub const USER_DATA_BASE: usize = 0x4010_0000;

/// VA for the 4 KiB user stack. The mapped page is at USER_STACK_BASE and
/// sp starts at USER_STACK_TOP (top of that page). We place it at the
/// very last slot of the code/data L0 so the whole user window fits in
/// a single 2 MiB L1 entry.
pub const USER_STACK_BASE: usize = 0x401F_F000;
pub const USER_STACK_TOP: usize = 0x4020_0000;

/// How many VA pages we reserve per category (sanity bound).
const USER_CODE_MAX_PAGES: usize = 16;
const USER_DATA_MAX_PAGES: usize = 16;

// ---------------------------------------------------------------------------
// Syscall numbers
// ---------------------------------------------------------------------------

pub const SYS_READ_NODE: usize = 1;
pub const SYS_PRINT: usize = 2;
pub const SYS_EXIT: usize = 3;

// Negative error codes (two's complement of Linux-style errno).
const EPERM: i64 = -1;
const ENOENT: i64 = -2;
const EINVAL: i64 = -3;

// ---------------------------------------------------------------------------
// Embedded user-mode demo program (position-independent RISC-V asm)
// ---------------------------------------------------------------------------
//
// This is the "hello world" of capability-enforced user space:
//   a0 on entry = readable_node_id (we have a read edge to it)
//   a1 on entry = forbidden_node_id (we do NOT have any edge to it)
//
//   1. Read the readable node into a stack buffer, then SYS_PRINT it.
//   2. Attempt to read the forbidden node -> expect EPERM in a0.
//   3. SYS_EXIT(0).
//
// The code uses only PC-relative branches plus immediate loads and
// register moves. No `la`/absolute refs, so the blob is safely
// relocatable from kernel .rodata to the user's VA.

global_asm!(
    r#"
.section .rodata.user_demo
.align 12
.globl _user_demo_start
_user_demo_start:
    # Save node ids across calls.
    mv    s0, a0                    # s0 = readable_node_id
    mv    s1, a1                    # s1 = forbidden_node_id

    # Reserve 128 bytes on the stack for a read buffer.
    addi  sp, sp, -128
    mv    s2, sp                    # s2 = buf

    # SYS_READ_NODE(s0, s2, 128)
    li    a7, 1
    mv    a0, s0
    mv    a1, s2
    li    a2, 128
    ecall

    # If a0 <= 0, skip the print.
    blez  a0, 10f

    # SYS_PRINT(s2, a0)
    mv    a2, a0
    mv    a0, s2
    mv    a1, a2
    li    a7, 2
    ecall
10:

    # SYS_READ_NODE(s1, s2, 128) -- forbidden node, should return -1 (EPERM).
    li    a7, 1
    mv    a0, s1
    mv    a1, s2
    li    a2, 128
    ecall
    # a0 is now -1; we simply exit after to let the kernel log the violation.

    # SYS_EXIT(0)
    li    a7, 3
    li    a0, 0
    ecall

    # Defensive hang in case SYS_EXIT ever returns.
20: j 20b

.globl _user_demo_end
.align 4
_user_demo_end:
"#
);

extern "C" {
    static _user_demo_start: u8;
    static _user_demo_end: u8;
    static _user_baddemo_start: u8;
    static _user_baddemo_end: u8;
}

/// Return the assembled demo program bytes (copied into a fresh user
/// page by `run_user_task`).
#[allow(static_mut_refs)]
pub fn demo_program_bytes() -> &'static [u8] {
    unsafe {
        let start = &_user_demo_start as *const u8;
        let end = &_user_demo_end as *const u8;
        let len = end.offset_from(start) as usize;
        core::slice::from_raw_parts(start, len)
    }
}

/// The "bad demo" program: deliberately dereferences a forbidden VA
/// (no edge -> no PTE -> MMU page fault) to exercise the hardware cap
/// enforcement path. Expected to be killed by the kernel.
#[allow(static_mut_refs)]
pub fn baddemo_program_bytes() -> &'static [u8] {
    unsafe {
        let start = &_user_baddemo_start as *const u8;
        let end = &_user_baddemo_end as *const u8;
        let len = end.offset_from(start) as usize;
        core::slice::from_raw_parts(start, len)
    }
}

// The bad-demo blob: loads from an unmapped VA so the MMU (not the
// syscall layer) catches the capability violation.
global_asm!(
    r#"
.section .rodata.user_demo
.align 12
.globl _user_baddemo_start
_user_baddemo_start:
    # Try to read a byte from an address we have no edge to. The user
    # address space maps only 0x40000000..0x40200000; anything else is
    # unmapped and will page-fault in U-mode.
    li    t0, 0x40300000            # forbidden VA
    lb    t1, 0(t0)                 # <-- load page fault, task killed
    # Unreachable:
    li    a7, 3
    li    a0, 42
    ecall
1:  j 1b
.globl _user_baddemo_end
.align 4
_user_baddemo_end:
"#
);

// ---------------------------------------------------------------------------
// Per-task address space
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Mapping {
    /// Source node id the mapping came from (0 for stack).
    node_id: u64,
    /// User virtual address base of this mapping.
    va: usize,
    /// Physical address (kernel direct address) of the backing frame.
    pa: usize,
    /// Edge label: "exec" / "read" / "write" / "stack".
    kind: &'static str,
}

struct UserAddressSpace {
    /// satp value (mode | ppn) for this user task.
    satp: usize,
    /// List of mappings for diagnostics.
    mappings: Vec<Mapping>,
    /// Entry VA (first exec edge).
    entry: usize,
}

/// Kernel-side snapshot of the user task we launched, so syscall handlers
/// can do capability checks and so fault/exit paths can long-jump back.
struct ActiveUserTask {
    /// The task's graph node id (for logging).
    task_node_id: u64,
    /// Allowed target node ids for 'read' or 'write' edges.
    read_allowed: Vec<u64>,
    /// Allowed target node ids for 'exec' edges.
    exec_allowed: Vec<u64>,
    /// Kernel long-jump context -- restored on exit/fault.
    kctx: *mut KernelCtx,
    /// Exit code recorded by SYS_EXIT (or synthesized on fault).
    exit_code: i64,
    /// Set to true if the task hit a cap violation / bad trap.
    faulted: bool,
}

static mut ACTIVE: Option<ActiveUserTask> = None;

fn active() -> Option<&'static ActiveUserTask> {
    #[allow(static_mut_refs)]
    unsafe { ACTIVE.as_ref() }
}

fn active_mut() -> Option<&'static mut ActiveUserTask> {
    #[allow(static_mut_refs)]
    unsafe { ACTIVE.as_mut() }
}

// ---------------------------------------------------------------------------
// Kernel long-jump (setjmp/longjmp) for returning to the kernel scheduler
// when a user task exits or faults.
// ---------------------------------------------------------------------------

#[repr(C)]
pub struct KernelCtx {
    // ra, sp, s0..s11 — all RISC-V callee-saved regs we need to preserve.
    pub ra: usize,
    pub sp: usize,
    pub s: [usize; 12],
    /// SATP value to restore (kernel PT).
    pub satp: usize,
}

impl KernelCtx {
    pub const fn zero() -> Self {
        Self { ra: 0, sp: 0, s: [0; 12], satp: 0 }
    }
}

global_asm!(
    r#"
# Save callee-saved registers + ra/sp into *a0. Returns 0 in a0 to the
# caller. If the kernel is later resumed via `user_longjmp(ctx, v)`, the
# 'return' appears in a0 from that caller with value `v` instead.
.align 4
.globl user_setjmp
user_setjmp:
    sd    ra,   0*8(a0)
    sd    sp,   1*8(a0)
    sd    s0,   2*8(a0)
    sd    s1,   3*8(a0)
    sd    s2,   4*8(a0)
    sd    s3,   5*8(a0)
    sd    s4,   6*8(a0)
    sd    s5,   7*8(a0)
    sd    s6,   8*8(a0)
    sd    s7,   9*8(a0)
    sd    s8,  10*8(a0)
    sd    s9,  11*8(a0)
    sd    s10, 12*8(a0)
    sd    s11, 13*8(a0)
    li    a0, 0
    ret

# Restore callee-saved registers + ra/sp from *a0, set a0 = a1 (nonzero),
# and jump to the saved ra. Also restores kernel SATP and clears sscratch.
.align 4
.globl user_longjmp
user_longjmp:
    ld    ra,   0*8(a0)
    ld    sp,   1*8(a0)
    ld    s0,   2*8(a0)
    ld    s1,   3*8(a0)
    ld    s2,   4*8(a0)
    ld    s3,   5*8(a0)
    ld    s4,   6*8(a0)
    ld    s5,   7*8(a0)
    ld    s6,   8*8(a0)
    ld    s7,   9*8(a0)
    ld    s8,  10*8(a0)
    ld    s9,  11*8(a0)
    ld    s10, 12*8(a0)
    ld    s11, 13*8(a0)
    ld    t0,  14*8(a0)             # saved kernel satp
    # Switch back to kernel page table.
    csrw  satp, t0
    sfence.vma zero, zero
    # Clear sscratch so S-mode traps use the current sp again.
    csrw  sscratch, zero
    # Return: a0 = a1 (the longjmp value).
    mv    a0, a1
    ret
"#
);

extern "C" {
    fn user_setjmp(ctx: *mut KernelCtx) -> usize;
    fn user_longjmp(ctx: *const KernelCtx, val: usize) -> !;
}

// ---------------------------------------------------------------------------
// Low-level entry to U-mode (sret after satp/sstatus/sepc/sp setup).
// ---------------------------------------------------------------------------

global_asm!(
    r#"
# void enter_usermode(u64 satp, u64 entry_pc, u64 user_sp, u64 arg0,
#                     u64 arg1, u64 kernel_sp_for_sscratch)
# a0 = satp, a1 = entry_pc, a2 = user_sp, a3 = arg0, a4 = arg1, a5 = ksp
.align 4
.globl enter_usermode_asm
enter_usermode_asm:
    # Install user page table.
    csrw  satp, a0
    sfence.vma zero, zero

    # Remember user entry/sp/arg0/arg1 before we clobber registers.
    mv    t0, a1                    # t0 = entry_pc
    mv    t1, a2                    # t1 = user_sp
    mv    t2, a3                    # t2 = arg0
    mv    t3, a4                    # t3 = arg1

    # Stash kernel sp in sscratch so trap entry can swap back.
    csrw  sscratch, a5

    # Flush icache so freshly-mapped code is coherent.
    fence.i

    # Program sstatus for return-to-U-mode:
    #   SPP  (bit 8)  = 0  (return to U)
    #   SPIE (bit 5)  = 0  (interrupts disabled in U-mode for M29)
    #   SUM  (bit 18) = 1  (S-mode can still access U memory — syscalls)
    csrr  t4, sstatus
    li    t5, 0x100                 # SPP mask
    not   t5, t5
    and   t4, t4, t5                # clear SPP
    li    t5, 0x20                  # SPIE mask
    not   t5, t5
    and   t4, t4, t5                # clear SPIE
    li    t5, 0x40000               # SUM mask
    or    t4, t4, t5                # set SUM
    csrw  sstatus, t4

    # sepc = user entry PC.
    csrw  sepc, t0

    # Zero caller-saved regs so the user doesn't inherit kernel state.
    # (Except a0/a1 which carry the syscall arguments we want to pass.)
    li    ra, 0
    li    gp, 0
    li    tp, 0
    li    t5, 0
    li    t6, 0
    li    s0, 0
    li    s1, 0
    li    s2, 0
    li    s3, 0
    li    s4, 0
    li    s5, 0
    li    s6, 0
    li    s7, 0
    li    s8, 0
    li    s9, 0
    li    s10, 0
    li    s11, 0
    li    a2, 0
    li    a3, 0
    li    a4, 0
    li    a5, 0
    li    a6, 0
    li    a7, 0

    # User sp and argument registers.
    mv    sp, t1
    mv    a0, t2
    mv    a1, t3

    sret
"#
);

extern "C" {
    fn enter_usermode_asm(
        satp: usize,
        entry_pc: usize,
        user_sp: usize,
        arg0: usize,
        arg1: usize,
        kernel_sp: usize,
    ) -> !;
}

// ---------------------------------------------------------------------------
// Building the user page table
// ---------------------------------------------------------------------------

/// Build a per-task Sv39 page table:
/// - Copies the kernel's MMIO gigapage (L2[0]) and RAM branch (L2[2]) so
///   the kernel can service traps even while this user's satp is active.
/// - Walks the task's outgoing edges. For each `exec`/`read`/`write`
///   edge, copies the target node's content into fresh 4 KiB frames and
///   maps them at USER_CODE_BASE / USER_DATA_BASE with R/X or R(/W) plus
///   the PTE_U bit.
/// - Allocates a fresh stack frame at USER_STACK_BASE (R/W/U).
///
/// Returns the user address space.
fn build_user_address_space(task_node_id: u64) -> Result<UserAddressSpace, &'static str> {
    let g = graph::get();
    let task = g.get_node(task_node_id).ok_or("task node not found")?;

    // Gather edges by label.
    let mut exec_targets: Vec<u64> = Vec::new();
    let mut read_targets: Vec<u64> = Vec::new();
    let mut write_targets: Vec<u64> = Vec::new();
    for edge in &task.edges {
        match edge.label.as_str() {
            "exec" => exec_targets.push(edge.target),
            "read" => read_targets.push(edge.target),
            "write" => write_targets.push(edge.target),
            _ => {} // ignore "child", "parent", "traverse", etc.
        }
    }

    if exec_targets.is_empty() {
        return Err("user task has no exec edge");
    }

    // Allocate the root PT and copy kernel L2 entries 0 and 2.
    let root = alloc_page_table();
    let root_pa = root as *const PageTable as usize;
    unsafe {
        let kroot = crate::mm::kernel_root_pa() as *const PageTable;
        assert!(!kroot.is_null(), "kernel root pa not initialized");
        (*root).entries[0] = (*kroot).entries[0]; // MMIO gigapage
        (*root).entries[2] = (*kroot).entries[2]; // RAM L1 branch
    }

    // Allocate L1 and L0 for the user 1 GiB window at VA 0x4000_0000.
    // Root L2 index 1 covers VA 0x4000_0000..0x8000_0000.
    let l1 = alloc_page_table();
    let l0 = alloc_page_table();
    root.entries[1] = PageTableEntry::branch(((l1 as *const _ as usize) >> 12) as u64);
    // All user VAs fit in L1[0] (VA 0x4000_0000..0x4020_0000, one 2 MiB
    // region backed by a single L0 table of 512 * 4 KiB entries).
    l1.entries[0] = PageTableEntry::branch(((l0 as *const _ as usize) >> 12) as u64);

    let mut mappings: Vec<Mapping> = Vec::new();

    // --- Map exec edges starting at USER_CODE_BASE. --------------------
    let mut code_slot = 0usize;
    let mut entry: Option<usize> = None;
    for (i, &tgt_id) in exec_targets.iter().enumerate() {
        if i >= USER_CODE_MAX_PAGES { break; }
        let tgt = match g.get_node(tgt_id) {
            Some(n) => n,
            None => continue,
        };
        let frame_pa = alloc_user_frame();
        let content = &tgt.content;
        let copy_len = core::cmp::min(content.len(), 4096);
        unsafe {
            core::ptr::copy_nonoverlapping(
                content.as_ptr(),
                frame_pa as *mut u8,
                copy_len,
            );
        }
        // R + X + U flags. No W — code pages are truly execute-only-ish.
        let flags = PTE_R | PTE_X | PTE_U;
        l0.entries[code_slot] = PageTableEntry::leaf((frame_pa >> 12) as u64, flags);
        let va = USER_CODE_BASE + code_slot * 4096;
        mappings.push(Mapping { node_id: tgt_id, va, pa: frame_pa, kind: "exec" });
        if entry.is_none() {
            entry = Some(va);
        }
        code_slot += 1;
    }

    // --- Map read edges starting at USER_DATA_BASE. --------------------
    // USER_DATA_BASE = 0x4010_0000, which is L0 index 256 (0x100 * 0x1000).
    let data_start = (USER_DATA_BASE - USER_CODE_BASE) / 4096; // = 256
    let mut data_slot = 0usize;
    for &tgt_id in read_targets.iter() {
        if data_slot >= USER_DATA_MAX_PAGES { break; }
        let tgt = match g.get_node(tgt_id) {
            Some(n) => n,
            None => continue,
        };
        let frame_pa = alloc_user_frame();
        let copy_len = core::cmp::min(tgt.content.len(), 4096);
        unsafe {
            core::ptr::copy_nonoverlapping(
                tgt.content.as_ptr(),
                frame_pa as *mut u8,
                copy_len,
            );
        }
        let flags = PTE_R | PTE_U;
        l0.entries[data_start + data_slot] = PageTableEntry::leaf((frame_pa >> 12) as u64, flags);
        mappings.push(Mapping {
            node_id: tgt_id,
            va: USER_DATA_BASE + data_slot * 4096,
            pa: frame_pa,
            kind: "read",
        });
        data_slot += 1;
    }

    // --- Map write edges after read edges. -----------------------------
    for &tgt_id in write_targets.iter() {
        if data_slot >= USER_DATA_MAX_PAGES { break; }
        let tgt = match g.get_node(tgt_id) {
            Some(n) => n,
            None => continue,
        };
        let frame_pa = alloc_user_frame();
        let copy_len = core::cmp::min(tgt.content.len(), 4096);
        unsafe {
            core::ptr::copy_nonoverlapping(
                tgt.content.as_ptr(),
                frame_pa as *mut u8,
                copy_len,
            );
        }
        let flags = PTE_R | PTE_W | PTE_U;
        l0.entries[data_start + data_slot] = PageTableEntry::leaf((frame_pa >> 12) as u64, flags);
        mappings.push(Mapping {
            node_id: tgt_id,
            va: USER_DATA_BASE + data_slot * 4096,
            pa: frame_pa,
            kind: "write",
        });
        data_slot += 1;
    }

    // --- Map the user stack: 1 page at USER_STACK_BASE (R/W/U). --------
    let stack_pa = alloc_user_frame();
    let stack_slot = (USER_STACK_BASE - USER_CODE_BASE) / 4096; // = 511
    l0.entries[stack_slot] = PageTableEntry::leaf(
        (stack_pa >> 12) as u64,
        PTE_R | PTE_W | PTE_U,
    );
    mappings.push(Mapping { node_id: 0, va: USER_STACK_BASE, pa: stack_pa, kind: "stack" });

    let satp = SATP_MODE_SV39 | (root_pa >> 12);

    Ok(UserAddressSpace {
        satp,
        mappings,
        entry: entry.ok_or("no entry point after mapping")?,
    })
}

// ---------------------------------------------------------------------------
// Spawn / run
// ---------------------------------------------------------------------------

/// Unique suffix for synthesized user-task node names.
static USER_TASK_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Run a user task from a code node id. This creates a fresh task node in
/// the graph, wires up the capability edges for the demo, builds the user
/// page table, drops to U-mode, and returns when the task exits or faults.
///
/// Returns the exit code (0 on success) or a negative value on fault.
pub fn run_user_task_from_code_node(
    code_node_id: u64,
    readable_node_id: u64,
    forbidden_node_id: u64,
) -> i64 {
    // Create the task node and wire up capability edges.
    let task_node_id = {
        let g = graph::get_mut();
        let n = USER_TASK_COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = alloc::format!("user-task-{}", n);
        let id = g.create_node(NodeType::System, &name);
        g.add_edge(1, "child", id); // attach under root for visibility

        // The core M29 claim: edges = capabilities.
        if !g.add_edge(id, "exec", code_node_id) {
            crate::println!("[user] couldn't add exec edge to node {}", code_node_id);
            return -1;
        }
        if readable_node_id != 0 {
            g.add_edge(id, "read", readable_node_id);
        }
        // NOTE: we deliberately do NOT add an edge to forbidden_node_id.

        // Stash a description in the task's content.
        if let Some(node) = g.get_node_mut(id) {
            let info = alloc::format!(
                "user task (M29)\nexec: {}\nread: {}\nforbidden: {}\n",
                code_node_id, readable_node_id, forbidden_node_id
            );
            node.content = info.into_bytes();
        }
        id
    };

    // Build the user address space.
    let aspace = match build_user_address_space(task_node_id) {
        Ok(a) => a,
        Err(e) => {
            crate::println!("[user] build_user_address_space failed: {}", e);
            return -1;
        }
    };

    crate::println!(
        "[user] task #{} mapped {} regions, entry={:#x}, satp={:#018x}",
        task_node_id, aspace.mappings.len(), aspace.entry, aspace.satp
    );
    for m in &aspace.mappings {
        if m.kind == "stack" {
            crate::println!(
                "       stack: va={:#010x} pa={:#010x} (R/W/U)",
                m.va, m.pa,
            );
        } else {
            crate::println!(
                "       {:5}: va={:#010x} pa={:#010x} node={}",
                m.kind, m.va, m.pa, m.node_id,
            );
        }
    }

    // Snapshot capability sets for syscall/fault checks.
    let (exec_allowed, read_allowed) = {
        let g = graph::get();
        let task = g.get_node(task_node_id).expect("task node vanished");
        let mut exec = Vec::new();
        let mut read = Vec::new();
        for e in &task.edges {
            match e.label.as_str() {
                "exec" => exec.push(e.target),
                "read" | "write" => read.push(e.target),
                _ => {}
            }
        }
        (exec, read)
    };

    // Prepare the setjmp context and install ActiveUserTask.
    let mut kctx = KernelCtx::zero();
    kctx.satp = arch::read_satp();
    let kctx_ptr: *mut KernelCtx = &mut kctx;

    unsafe {
        ACTIVE = Some(ActiveUserTask {
            task_node_id,
            read_allowed,
            exec_allowed,
            kctx: kctx_ptr,
            exit_code: 0,
            faulted: false,
        });
    }

    // setjmp: on the initial return (0) we drop to U-mode. On longjmp
    // from the trap handler we come back here with a nonzero value.
    let jmp = unsafe { user_setjmp(kctx_ptr) };
    if jmp == 0 {
        // Drop to U-mode. On exit/fault we'll come back via user_longjmp.
        // We pass the ACTIVE kctx.sp at this point as the kernel sp for
        // sscratch. enter_usermode_asm will install it and `sret`.
        //
        // Wait — we want sscratch = _current_ kernel sp at the time of
        // the sret, not the saved jmp buf. Just pass current sp (we'll
        // use a small helper below).
        unsafe {
            let cur_sp = current_sp();
            // Update kctx.satp so longjmp restores the current kernel pt.
            (*kctx_ptr).satp = arch::read_satp();
            enter_usermode_asm(
                aspace.satp,
                aspace.entry,
                USER_STACK_TOP,
                arg_for_u_mode_a0(task_node_id),
                arg_for_u_mode_a1(task_node_id),
                cur_sp,
            );
        }
    }

    // We're back in kernel context (longjmp'd from trap handler). Collect
    // results and clean up.
    let (code, faulted) = {
        let a = active().unwrap();
        (a.exit_code, a.faulted)
    };
    unsafe { ACTIVE = None; }

    // Mark the task node done.
    {
        let g = graph::get_mut();
        if let Some(node) = g.get_node_mut(task_node_id) {
            let info = alloc::format!(
                "user task (M29)\nexit: {}\nfaulted: {}\n",
                code, faulted,
            );
            node.content = info.into_bytes();
        }
    }

    if faulted {
        crate::println!("[user] task #{} killed by capability violation (exit={})", task_node_id, code);
    } else {
        crate::println!("[user] task #{} exited cleanly with code {}", task_node_id, code);
    }

    code
}

/// a0 at U-mode entry = the id of the task's first `read` edge target (if
/// any) — a well-known readable node.
fn arg_for_u_mode_a0(task_node_id: u64) -> usize {
    let g = graph::get();
    if let Some(task) = g.get_node(task_node_id) {
        for e in &task.edges {
            if e.label == "read" || e.label == "write" {
                return e.target as usize;
            }
        }
    }
    0
}

/// a1 at U-mode entry = a forbidden node id for the demo. We always pass 1
/// (root). The task has no edge to node 1, so the read_node syscall must
/// fail with EPERM — this is the core capability-enforcement demo.
fn arg_for_u_mode_a1(_task_node_id: u64) -> usize {
    1 // root — deliberately no edge from the task to it.
}

/// Read the current stack pointer.
#[inline(always)]
unsafe fn current_sp() -> usize {
    let sp: usize;
    core::arch::asm!("mv {}, sp", out(reg) sp);
    sp
}

// ---------------------------------------------------------------------------
// Syscall & fault handlers (invoked from trap::trap_handler)
// ---------------------------------------------------------------------------

/// Handle a U-mode ecall. The trap handler already advanced sepc.
pub fn handle_syscall(frame: &mut TrapFrame) {
    let nr = frame.a7();
    match nr {
        SYS_READ_NODE => {
            let node_id = frame.a0() as u64;
            let buf_va = frame.a1();
            let buf_len = frame.a2();
            let r = sys_read_node(node_id, buf_va, buf_len);
            frame.set_a0(r as usize);
        }
        SYS_PRINT => {
            let buf_va = frame.a0();
            let buf_len = frame.a1();
            let r = sys_print(buf_va, buf_len);
            frame.set_a0(r as usize);
        }
        SYS_EXIT => {
            let code = frame.a0() as i64;
            sys_exit(code);
            // sys_exit never returns.
        }
        _ => {
            crate::println!("[user] unknown syscall #{}", nr);
            frame.set_a0(EINVAL as usize);
        }
    }
}

/// Handle a non-ecall U-mode exception: cap violation (no mapping), bad
/// store/load/instruction access. Log + long-jump back to the kernel.
pub fn handle_user_fault(frame: &mut TrapFrame, cause: usize, stval: usize) {
    let cause_name = match cause {
        1 => "instruction access fault",
        2 => "illegal instruction",
        4 => "load address misaligned",
        5 => "load access fault",
        6 => "store address misaligned",
        7 => "store access fault",
        12 => "instruction page fault",
        13 => "load page fault",
        15 => "store/AMO page fault",
        _ => "unknown exception",
    };
    let task_id = active().map(|a| a.task_node_id).unwrap_or(0);
    crate::println!(
        "[user] capability violation: task #{} -- {} at va={:#x} pc={:#x}",
        task_id, cause_name, stval, frame.sepc
    );
    crate::println!("       (no edge grants access to this address)");

    // Record and longjmp.
    if let Some(a) = active_mut() {
        a.faulted = true;
        a.exit_code = -((cause as i64) + 100); // arbitrary fault code
        let ctx = a.kctx;
        unsafe { user_longjmp(ctx, 1); }
    }
    // Shouldn't get here.
    crate::println!("[user] no active user task -- halting");
    loop { unsafe { core::arch::asm!("wfi") }; }
}

// ---------------------------------------------------------------------------
// Individual syscall implementations
// ---------------------------------------------------------------------------

fn sys_read_node(node_id: u64, buf_va: usize, buf_len: usize) -> i64 {
    crate::println!(
        "[sys] SYS_READ_NODE(node={}, buf_va={:#x}, len={})",
        node_id, buf_va, buf_len,
    );
    // Capability check: does the current task have a read/write edge
    // to this node?
    let allowed = active()
        .map(|a| a.read_allowed.iter().any(|&t| t == node_id))
        .unwrap_or(false);
    if !allowed {
        let task_id = active().map(|a| a.task_node_id).unwrap_or(0);
        crate::println!(
            "[user] capability violation: task #{} tried to read node {} (no edge)",
            task_id, node_id
        );
        return EPERM;
    }

    // Fetch content.
    let g = graph::get();
    let node = match g.get_node(node_id) {
        Some(n) => n,
        None => return ENOENT,
    };
    let content = &node.content;
    let n = core::cmp::min(content.len(), buf_len);
    if n == 0 {
        return 0;
    }

    // Copy to user buffer. SUM is set, so we can directly access user VAs.
    // Defensive: check that buf_va + n doesn't wrap.
    if buf_va.checked_add(n).is_none() {
        return EINVAL;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(
            content.as_ptr(),
            buf_va as *mut u8,
            n,
        );
    }
    n as i64
}

fn sys_print(buf_va: usize, buf_len: usize) -> i64 {
    if buf_len == 0 {
        return 0;
    }
    if buf_va.checked_add(buf_len).is_none() {
        return EINVAL;
    }
    // Cap to something sane.
    let n = core::cmp::min(buf_len, 4096);
    // Read bytes out of user space and print.
    unsafe {
        let p = buf_va as *const u8;
        for i in 0..n {
            let b = core::ptr::read_volatile(p.add(i));
            crate::uart::putc(b);
        }
    }
    n as i64
}

fn sys_exit(code: i64) -> ! {
    let (ctx, id) = {
        let a = active_mut().expect("sys_exit with no active user task");
        a.exit_code = code;
        a.faulted = false;
        (a.kctx, a.task_node_id)
    };
    crate::println!("[user] task #{} SYS_EXIT({})", id, code);
    unsafe { user_longjmp(ctx, 1); }
}

// ---------------------------------------------------------------------------
// Boot-time demo graph setup
// ---------------------------------------------------------------------------

/// Demo node ids, populated by `init()`. The shell's `spawn` command uses
/// these when no argument is given.
static mut DEMO_CODE_ID: u64 = 0;
static mut BADDEMO_CODE_ID: u64 = 0;
static mut DEMO_TEXT_ID: u64 = 0;

/// Initialize the demo user-space nodes: a Binary code node (containing
/// the assembled program) and a Text node the task will be allowed to
/// read via its `read` edge. Plus a "bad" code node that trips the MMU.
#[allow(static_mut_refs)]
pub fn init() {
    let bytes = demo_program_bytes();
    let bad_bytes = baddemo_program_bytes();
    let g = graph::get_mut();
    let code_id = g.create_node(NodeType::Binary, "user-demo-code");
    if let Some(node) = g.get_node_mut(code_id) {
        node.content = bytes.to_vec();
    }
    g.add_edge(1, "child", code_id);

    let bad_id = g.create_node(NodeType::Binary, "user-baddemo-code");
    if let Some(node) = g.get_node_mut(bad_id) {
        node.content = bad_bytes.to_vec();
    }
    g.add_edge(1, "child", bad_id);

    let text_id = g.create_node(NodeType::Text, "user-demo-text");
    if let Some(node) = g.get_node_mut(text_id) {
        node.content = b"Hello from Helios U-mode! Edges are capabilities.\n"
            .to_vec();
    }
    g.add_edge(1, "child", text_id);

    unsafe {
        DEMO_CODE_ID = code_id;
        BADDEMO_CODE_ID = bad_id;
        DEMO_TEXT_ID = text_id;
    }
    crate::println!(
        "[user] demo nodes ready: code=#{} ({} bytes), baddemo=#{} ({} bytes), text=#{}",
        code_id, bytes.len(), bad_id, bad_bytes.len(), text_id,
    );
}

#[allow(static_mut_refs)]
pub fn demo_code_id() -> u64 { unsafe { DEMO_CODE_ID } }
#[allow(static_mut_refs)]
pub fn baddemo_code_id() -> u64 { unsafe { BADDEMO_CODE_ID } }
#[allow(static_mut_refs)]
pub fn demo_text_id() -> u64 { unsafe { DEMO_TEXT_ID } }
