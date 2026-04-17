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
    PTE_R, PTE_U, PTE_V, PTE_W, PTE_X,
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
///
/// Code pages occupy L0 indices 0..USER_CODE_MAX_PAGES; data pages start
/// at L0 index 256 (derived from USER_DATA_BASE). Code and data must not
/// collide, so USER_CODE_MAX_PAGES < 256. 64 pages (256 KiB) is enough
/// for native Rust programs (M31); the asm demos use just 1.
const USER_CODE_MAX_PAGES: usize = 64;
const USER_DATA_MAX_PAGES: usize = 16;

// ---------------------------------------------------------------------------
// Syscall numbers
// ---------------------------------------------------------------------------

pub const SYS_READ_NODE: usize = 1;
pub const SYS_PRINT: usize = 2;
pub const SYS_EXIT: usize = 3;
// M30 additions:
pub const SYS_WRITE_NODE: usize = 4;
pub const SYS_LIST_EDGES: usize = 5;
pub const SYS_FOLLOW_EDGE: usize = 6;
pub const SYS_SELF: usize = 7;
// M33 addition:
/// Ask the kernel for a fresh, zeroed writable memory region. Creates a
/// `NodeType::Memory` node, allocates backing frames, adds a `write`
/// edge from the caller to the new node, and maps the frames into the
/// caller's data VA window. Returns the user VA of the first page.
pub const SYS_MAP_NODE: usize = 8;
// M34 addition:
/// Read the full string label of an outgoing edge by index. Closes the
/// "everything shows as ?" gap in `SYS_LIST_EDGES`, which only surfaces
/// cap-kind bytes — structural labels like `child` / `parent` / `self`
/// were reported as Unknown. Cap: same `traverse` edge the caller
/// already needs for `SYS_LIST_EDGES`.
pub const SYS_READ_EDGE_LABEL: usize = 9;

// Negative error codes (two's complement of Linux-style errno).
const EPERM: i64 = -1;
const ENOENT: i64 = -2;
const EINVAL: i64 = -3;
// M33 addition — no backing frames or no contiguous VA slots available.
const ENOMEM: i64 = -4;

// ---------------------------------------------------------------------------
// Edge-label-kind codes exposed to user mode via SYS_LIST_EDGES.
// ---------------------------------------------------------------------------
const EDGE_KIND_UNKNOWN: u8 = 0;
const EDGE_KIND_READ: u8 = 1;
const EDGE_KIND_WRITE: u8 = 2;
const EDGE_KIND_EXEC: u8 = 3;
const EDGE_KIND_TRAVERSE: u8 = 4;

fn label_to_kind(label: &str) -> u8 {
    match label {
        "read" => EDGE_KIND_READ,
        "write" => EDGE_KIND_WRITE,
        "exec" => EDGE_KIND_EXEC,
        "traverse" => EDGE_KIND_TRAVERSE,
        _ => EDGE_KIND_UNKNOWN,
    }
}

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
    static _user_who_start: u8;
    static _user_who_end: u8;
    static _user_explorer_start: u8;
    static _user_explorer_end: u8;
    static _user_editor_start: u8;
    static _user_editor_end: u8;
    static _user_naughty_start: u8;
    static _user_naughty_end: u8;
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

#[allow(static_mut_refs)]
pub fn who_program_bytes() -> &'static [u8] {
    unsafe {
        let s = &_user_who_start as *const u8;
        let e = &_user_who_end as *const u8;
        core::slice::from_raw_parts(s, e.offset_from(s) as usize)
    }
}

#[allow(static_mut_refs)]
pub fn explorer_program_bytes() -> &'static [u8] {
    unsafe {
        let s = &_user_explorer_start as *const u8;
        let e = &_user_explorer_end as *const u8;
        core::slice::from_raw_parts(s, e.offset_from(s) as usize)
    }
}

#[allow(static_mut_refs)]
pub fn editor_program_bytes() -> &'static [u8] {
    unsafe {
        let s = &_user_editor_start as *const u8;
        let e = &_user_editor_end as *const u8;
        core::slice::from_raw_parts(s, e.offset_from(s) as usize)
    }
}

#[allow(static_mut_refs)]
pub fn naughty_program_bytes() -> &'static [u8] {
    unsafe {
        let s = &_user_naughty_start as *const u8;
        let e = &_user_naughty_end as *const u8;
        core::slice::from_raw_parts(s, e.offset_from(s) as usize)
    }
}

// ---------------------------------------------------------------------------
// M31: `hello-user` — the first linker-placed Rust binary running in U-mode.
//
// The bytes come from `crates/hello-user/src/main.rs` (linking against
// helios-std), compiled by `build.rs` into a raw `-O binary` blob and
// included at compile time. See `docs/userspace/rust-std.md` for the
// design, `crates/hello-user/linker.ld` for the layout, and
// `docs/design/capability-edges.md` for the syscall ABI this program
// consumes.
//
// The blob is bigger than one page (program + 64 KiB bump heap ≈ 70 KiB),
// so the kernel's multi-page exec-edge mapping is what makes this work;
// see `build_user_address_space` above.
// ---------------------------------------------------------------------------

static HELLO_USER_BIN: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/user-bins/hello-user.bin",
));

/// Raw bytes of the `hello-user` Rust user program.
pub fn hello_program_bytes() -> &'static [u8] {
    HELLO_USER_BIN
}

// ---------------------------------------------------------------------------
// M32: graph-native Rust user programs (ls, cat).
//
// Same pattern as HELLO_USER_BIN — each lives in its own crate under
// crates/, built by build.rs into OUT_DIR/user-bins/<name>.bin, and is
// embedded here via include_bytes!.
// ---------------------------------------------------------------------------

static LS_USER_BIN: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/user-bins/ls-user.bin",
));

/// Raw bytes of the `ls-user` graph-native listing program.
pub fn ls_program_bytes() -> &'static [u8] {
    LS_USER_BIN
}

static CAT_USER_BIN: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/user-bins/cat-user.bin",
));

/// Raw bytes of the `cat-user` graph-native content-read program.
pub fn cat_program_bytes() -> &'static [u8] {
    CAT_USER_BIN
}

// ---------------------------------------------------------------------------
// M33: `mmap-user` — exercises SYS_MAP_NODE.
//
// Same embedding pattern as the M31/M32 binaries. The demo program
// calls `map_node` for a 32 KiB slab and an 8 KiB slab, fills each
// with a distinct pattern, verifies readback, and checks the two VAs
// don't overlap. See `crates/mmap-user/src/main.rs` for the
// implementation.
// ---------------------------------------------------------------------------

static MMAP_USER_BIN: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/user-bins/mmap-user.bin",
));

/// Raw bytes of the `mmap-user` SYS_MAP_NODE demo program.
pub fn mmap_program_bytes() -> &'static [u8] {
    MMAP_USER_BIN
}

// ---------------------------------------------------------------------------
// M33.5: `bigalloc-user` — proves helios-std's GlobalAlloc is backed by
// SYS_MAP_NODE slabs. Allocates a big Vec, then a bigger Vec, and
// verifies two write-edges to Memory nodes appear on the task.
// See `crates/bigalloc-user/src/main.rs`.
// ---------------------------------------------------------------------------

static BIGALLOC_USER_BIN: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/user-bins/bigalloc-user.bin",
));

/// Raw bytes of the `bigalloc-user` GlobalAlloc smoke-test program.
pub fn bigalloc_program_bytes() -> &'static [u8] {
    BIGALLOC_USER_BIN
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
// M30 demo blobs — each is position-independent and fits in one 4 KiB page.
// All branch targets are local labels; no absolute addresses.
// ---------------------------------------------------------------------------

// "who am i?" — SYS_SELF + SYS_PRINT.
// No args. Prints "i am task #<id>\n".
global_asm!(
    r#"
.section .rodata.user_demo
.align 12
.globl _user_who_start
_user_who_start:
    # SYS_SELF -> a0 = my task node id
    li    a7, 7
    ecall
    mv    s0, a0                   # s0 = task id

    # sp -= 64 for a scratch buffer
    addi  sp, sp, -64
    mv    s1, sp                   # s1 = buf base

    # Prefix "i am task #"  (11 chars)
    li    t0, 105; sb t0, 0(s1)    # 'i'
    li    t0, 32;  sb t0, 1(s1)    # ' '
    li    t0, 97;  sb t0, 2(s1)    # 'a'
    li    t0, 109; sb t0, 3(s1)    # 'm'
    li    t0, 32;  sb t0, 4(s1)    # ' '
    li    t0, 116; sb t0, 5(s1)    # 't'
    li    t0, 97;  sb t0, 6(s1)    # 'a'
    li    t0, 115; sb t0, 7(s1)    # 's'
    li    t0, 107; sb t0, 8(s1)    # 'k'
    li    t0, 32;  sb t0, 9(s1)    # ' '
    li    t0, 35;  sb t0, 10(s1)   # '#'

    addi  s2, s1, 11               # s2 = output cursor

    # itoa via repeated subtraction (no M extension needed).
    # At each outer iteration: subtract 10 from t2 until t2 < 10; t5
    # accumulates the quotient. After the inner loop, t2 is the digit,
    # and t5 is the next n.
    addi  t3, s1, 40               # t3 = reverse base
    mv    t4, t3                   # t4 = reverse cursor
    mv    t2, s0                   # t2 = n
    bnez  t2, who_itoa_outer
    li    t5, 48; sb t5, 0(t4); addi t4, t4, 1
    j     who_itoa_done
who_itoa_outer:
    li    t5, 0                    # t5 = quotient accumulator
    li    t6, 10
who_itoa_inner:
    bltu  t2, t6, who_itoa_emit
    sub   t2, t2, t6
    addi  t5, t5, 1
    j     who_itoa_inner
who_itoa_emit:
    addi  t2, t2, 48               # digit = t2 + '0'
    sb    t2, 0(t4)
    addi  t4, t4, 1
    mv    t2, t5                   # n := quotient
    bnez  t2, who_itoa_outer
who_itoa_done:
    # Reverse-copy [t3..t4) into [s2..)
who_rev:
    beq   t4, t3, who_rev_done
    addi  t4, t4, -1
    lb    t5, 0(t4)
    sb    t5, 0(s2)
    addi  s2, s2, 1
    j     who_rev
who_rev_done:
    # Append '\n'
    li    t0, 10
    sb    t0, 0(s2)
    addi  s2, s2, 1

    # SYS_PRINT(buf, len = s2 - s1)
    sub   a1, s2, s1
    mv    a0, s1
    li    a7, 2
    ecall

    # SYS_EXIT(0)
    li    a7, 3
    li    a0, 0
    ecall
who_hang: j who_hang
.globl _user_who_end
.align 4
_user_who_end:
"#
);

// "explorer" — SYS_SELF, then SYS_LIST_EDGES(self) and print each target+label.
// Requires a `traverse` edge to self (added at spawn time).
global_asm!(
    r#"
.section .rodata.user_demo
.align 12
.globl _user_explorer_start
_user_explorer_start:
    # SYS_SELF -> s0 = my id
    li    a7, 7
    ecall
    mv    s0, a0

    # Allocate scratch on stack: 512 bytes.
    # Layout: [edge_buf 256B | string_buf 256B]
    addi  sp, sp, -512
    mv    s1, sp                   # s1 = edge buf
    addi  s2, s1, 256              # s2 = string buf base

    # SYS_LIST_EDGES(self, edge_buf, 16 entries)
    mv    a0, s0
    mv    a1, s1
    li    a2, 16
    li    a7, 5
    ecall
    blez  a0, exp_done
    mv    s3, a0                   # s3 = edge count
    mv    s4, s1                   # s4 = entry pointer
    li    s5, 0                    # s5 = i
exp_loop:
    bge   s5, s3, exp_done
    mv    t0, s2                   # t0 = cursor

    # two spaces prefix
    li    t1, 32; sb t1, 0(t0); addi t0, t0, 1
    li    t1, 32; sb t1, 0(t0); addi t0, t0, 1
    # hash marker
    li    t1, 35; sb t1, 0(t0); addi t0, t0, 1

    # target id at entry[0..8] (u64)
    ld    t2, 0(s4)

    # itoa into reverse scratch at s2 + 192 (no M extension).
    # Outer iter: subtract 10 from t2 until t2<10. t2 becomes the digit,
    # t5 holds the quotient for the next iteration.
    addi  t3, s2, 192
    mv    t4, t3
    bnez  t2, exp_itoa_outer
    li    t5, 48; sb t5, 0(t4); addi t4, t4, 1
    j     exp_itoa_done
exp_itoa_outer:
    li    t5, 0
    li    t6, 10
exp_itoa_inner:
    bltu  t2, t6, exp_itoa_emit
    sub   t2, t2, t6
    addi  t5, t5, 1
    j     exp_itoa_inner
exp_itoa_emit:
    addi  t2, t2, 48
    sb    t2, 0(t4)
    addi  t4, t4, 1
    mv    t2, t5
    bnez  t2, exp_itoa_outer
exp_itoa_done:
exp_rev:
    beq   t4, t3, exp_rev_done
    addi  t4, t4, -1
    lb    t5, 0(t4)
    sb    t5, 0(t0)
    addi  t0, t0, 1
    j     exp_rev
exp_rev_done:
    # " label="
    li    t1, 32;  sb t1, 0(t0); addi t0, t0, 1
    li    t1, 108; sb t1, 0(t0); addi t0, t0, 1  # 'l'
    li    t1, 97;  sb t1, 0(t0); addi t0, t0, 1  # 'a'
    li    t1, 98;  sb t1, 0(t0); addi t0, t0, 1  # 'b'
    li    t1, 101; sb t1, 0(t0); addi t0, t0, 1  # 'e'
    li    t1, 108; sb t1, 0(t0); addi t0, t0, 1  # 'l'
    li    t1, 61;  sb t1, 0(t0); addi t0, t0, 1  # '='

    # label_kind at entry[8] (u8)
    lbu   t2, 8(s4)
    li    t5, 1
    beq   t2, t5, exp_lab_r
    li    t5, 2
    beq   t2, t5, exp_lab_w
    li    t5, 3
    beq   t2, t5, exp_lab_x
    li    t5, 4
    beq   t2, t5, exp_lab_t
    li    t1, 63;  sb t1, 0(t0); addi t0, t0, 1  # '?'
    j     exp_lab_done
exp_lab_r:
    li    t1, 114; sb t1, 0(t0); addi t0, t0, 1  # 'r'
    j     exp_lab_done
exp_lab_w:
    li    t1, 119; sb t1, 0(t0); addi t0, t0, 1  # 'w'
    j     exp_lab_done
exp_lab_x:
    li    t1, 120; sb t1, 0(t0); addi t0, t0, 1  # 'x'
    j     exp_lab_done
exp_lab_t:
    li    t1, 116; sb t1, 0(t0); addi t0, t0, 1  # 't'
exp_lab_done:
    # '\n'
    li    t1, 10; sb t1, 0(t0); addi t0, t0, 1

    sub   a1, t0, s2
    mv    a0, s2
    li    a7, 2
    ecall

    # Advance: s4 += 16, s5 += 1
    addi  s4, s4, 16
    addi  s5, s5, 1
    j     exp_loop

exp_done:
    li    a7, 3
    li    a0, 0
    ecall
exp_hang: j exp_hang
.globl _user_explorer_end
.align 4
_user_explorer_end:
"#
);

// "editor" — reads scratch node, prints, writes new content, reads+prints again.
// Needs `read` + `write` edges to the scratch node.
// a0 on entry = scratch node id.
global_asm!(
    r#"
.section .rodata.user_demo
.align 12
.globl _user_editor_start
_user_editor_start:
    mv    s0, a0                   # s0 = scratch id

    # Stack scratch: 128B read buffer + 64B message buffer.
    addi  sp, sp, -192
    mv    s1, sp                   # s1 = read buf
    addi  s2, s1, 128              # s2 = message buf

    # Print "editor: BEFORE:\n"  (len=16)
    li    t0, 101; sb t0, 0(s2)    # 'e'
    li    t0, 100; sb t0, 1(s2)    # 'd'
    li    t0, 105; sb t0, 2(s2)    # 'i'
    li    t0, 116; sb t0, 3(s2)    # 't'
    li    t0, 111; sb t0, 4(s2)    # 'o'
    li    t0, 114; sb t0, 5(s2)    # 'r'
    li    t0, 58;  sb t0, 6(s2)    # ':'
    li    t0, 32;  sb t0, 7(s2)    # ' '
    li    t0, 66;  sb t0, 8(s2)    # 'B'
    li    t0, 69;  sb t0, 9(s2)    # 'E'
    li    t0, 70;  sb t0, 10(s2)   # 'F'
    li    t0, 79;  sb t0, 11(s2)   # 'O'
    li    t0, 82;  sb t0, 12(s2)   # 'R'
    li    t0, 69;  sb t0, 13(s2)   # 'E'
    li    t0, 58;  sb t0, 14(s2)   # ':'
    li    t0, 10;  sb t0, 15(s2)   # '\n'
    mv    a0, s2
    li    a1, 16
    li    a7, 2
    ecall

    # SYS_READ_NODE(s0, s1, 128)
    li    a7, 1
    mv    a0, s0
    mv    a1, s1
    li    a2, 128
    ecall
    blez  a0, ed_skip_b
    mv    a2, a0
    mv    a0, s1
    mv    a1, a2
    li    a7, 2
    ecall
ed_skip_b:

    # Build new content in s1: "edited by syscall!\n" (19 bytes)
    li    t0, 101; sb t0, 0(s1)    # 'e'
    li    t0, 100; sb t0, 1(s1)    # 'd'
    li    t0, 105; sb t0, 2(s1)    # 'i'
    li    t0, 116; sb t0, 3(s1)    # 't'
    li    t0, 101; sb t0, 4(s1)    # 'e'
    li    t0, 100; sb t0, 5(s1)    # 'd'
    li    t0, 32;  sb t0, 6(s1)    # ' '
    li    t0, 98;  sb t0, 7(s1)    # 'b'
    li    t0, 121; sb t0, 8(s1)    # 'y'
    li    t0, 32;  sb t0, 9(s1)    # ' '
    li    t0, 115; sb t0, 10(s1)   # 's'
    li    t0, 121; sb t0, 11(s1)   # 'y'
    li    t0, 115; sb t0, 12(s1)   # 's'
    li    t0, 99;  sb t0, 13(s1)   # 'c'
    li    t0, 97;  sb t0, 14(s1)   # 'a'
    li    t0, 108; sb t0, 15(s1)   # 'l'
    li    t0, 108; sb t0, 16(s1)   # 'l'
    li    t0, 33;  sb t0, 17(s1)   # '!'
    li    t0, 10;  sb t0, 18(s1)   # '\n'

    # SYS_WRITE_NODE(s0, s1, 19)
    li    a7, 4
    mv    a0, s0
    mv    a1, s1
    li    a2, 19
    ecall
    # (ignore return value — demo still exits cleanly either way)

    # "editor: AFTER:\n" (len=15)
    li    t0, 101; sb t0, 0(s2)    # 'e'
    li    t0, 100; sb t0, 1(s2)    # 'd'
    li    t0, 105; sb t0, 2(s2)    # 'i'
    li    t0, 116; sb t0, 3(s2)    # 't'
    li    t0, 111; sb t0, 4(s2)    # 'o'
    li    t0, 114; sb t0, 5(s2)    # 'r'
    li    t0, 58;  sb t0, 6(s2)    # ':'
    li    t0, 32;  sb t0, 7(s2)    # ' '
    li    t0, 65;  sb t0, 8(s2)    # 'A'
    li    t0, 70;  sb t0, 9(s2)    # 'F'
    li    t0, 84;  sb t0, 10(s2)   # 'T'
    li    t0, 69;  sb t0, 11(s2)   # 'E'
    li    t0, 82;  sb t0, 12(s2)   # 'R'
    li    t0, 58;  sb t0, 13(s2)   # ':'
    li    t0, 10;  sb t0, 14(s2)   # '\n'
    mv    a0, s2
    li    a1, 15
    li    a7, 2
    ecall

    # Re-read and print
    li    a7, 1
    mv    a0, s0
    mv    a1, s1
    li    a2, 128
    ecall
    blez  a0, ed_skip_a
    mv    a2, a0
    mv    a0, s1
    mv    a1, a2
    li    a7, 2
    ecall
ed_skip_a:

    li    a7, 3
    li    a0, 0
    ecall
ed_hang: j ed_hang
.globl _user_editor_end
.align 4
_user_editor_end:
"#
);

// "naughty" — has `read` but NO `write` edge. Tries SYS_WRITE_NODE.
// Expects a0 == -1 (EPERM) and prints a message to that effect.
// a0 on entry = scratch node id.
global_asm!(
    r#"
.section .rodata.user_demo
.align 12
.globl _user_naughty_start
_user_naughty_start:
    mv    s0, a0                   # s0 = scratch id

    # Stack scratch.
    addi  sp, sp, -64
    mv    s1, sp

    # 1-byte write attempt
    li    t0, 88; sb t0, 0(s1)     # 'X'
    li    a7, 4
    mv    a0, s0
    mv    a1, s1
    li    a2, 1
    ecall
    mv    s2, a0                   # s2 = return code

    bltz  s2, nau_refused

    # Surprise path — shouldn't happen.
    li    t0, 79;  sb t0, 0(s1)    # 'O'
    li    t0, 79;  sb t0, 1(s1)    # 'O'
    li    t0, 80;  sb t0, 2(s1)    # 'P'
    li    t0, 83;  sb t0, 3(s1)    # 'S'
    li    t0, 10;  sb t0, 4(s1)    # '\n'
    mv    a0, s1
    li    a1, 5
    li    a7, 2
    ecall
    li    a7, 3
    li    a0, 99
    ecall
    j     nau_hang

nau_refused:
    # "EPERM (write refused)\n" -- 22 bytes
    li    t0, 69;  sb t0, 0(s1)    # 'E'
    li    t0, 80;  sb t0, 1(s1)    # 'P'
    li    t0, 69;  sb t0, 2(s1)    # 'E'
    li    t0, 82;  sb t0, 3(s1)    # 'R'
    li    t0, 77;  sb t0, 4(s1)    # 'M'
    li    t0, 32;  sb t0, 5(s1)    # ' '
    li    t0, 40;  sb t0, 6(s1)    # '('
    li    t0, 119; sb t0, 7(s1)    # 'w'
    li    t0, 114; sb t0, 8(s1)    # 'r'
    li    t0, 105; sb t0, 9(s1)    # 'i'
    li    t0, 116; sb t0, 10(s1)   # 't'
    li    t0, 101; sb t0, 11(s1)   # 'e'
    li    t0, 32;  sb t0, 12(s1)   # ' '
    li    t0, 114; sb t0, 13(s1)   # 'r'
    li    t0, 101; sb t0, 14(s1)   # 'e'
    li    t0, 102; sb t0, 15(s1)   # 'f'
    li    t0, 117; sb t0, 16(s1)   # 'u'
    li    t0, 115; sb t0, 17(s1)   # 's'
    li    t0, 101; sb t0, 18(s1)   # 'e'
    li    t0, 100; sb t0, 19(s1)   # 'd'
    li    t0, 41;  sb t0, 20(s1)   # ')'
    li    t0, 10;  sb t0, 21(s1)   # '\n'
    mv    a0, s1
    li    a1, 22
    li    a7, 2
    ecall
    li    a7, 3
    li    a0, 1
    ecall
nau_hang: j nau_hang
.globl _user_naughty_end
.align 4
_user_naughty_end:
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
    /// Kernel VA (== PA, identity-mapped) of the L0 table covering the
    /// user window `0x4000_0000..0x4020_0000`. `SYS_MAP_NODE` (M33) uses
    /// this to install fresh R+W+U leaf PTEs in the data-VA slot range
    /// without rebuilding the whole address space.
    l0_pa: usize,
}

/// Kernel-side snapshot of the user task we launched, so syscall handlers
/// can do capability checks and so fault/exit paths can long-jump back.
struct ActiveUserTask {
    /// The task's graph node id (for logging).
    task_node_id: u64,
    /// Allowed target node ids for 'read' or 'write' edges (readable).
    read_allowed: Vec<u64>,
    /// Allowed target node ids for 'write' edges (writable — for SYS_WRITE_NODE).
    write_allowed: Vec<u64>,
    /// Allowed target node ids for 'exec' edges.
    #[allow(dead_code)]
    exec_allowed: Vec<u64>,
    /// Allowed source node ids for 'traverse' edges (SYS_LIST_EDGES/FOLLOW).
    traverse_allowed: Vec<u64>,
    /// Kernel long-jump context -- restored on exit/fault.
    kctx: *mut KernelCtx,
    /// Exit code recorded by SYS_EXIT (or synthesized on fault).
    exit_code: i64,
    /// Set to true if the task hit a cap violation / bad trap.
    faulted: bool,
    /// Kernel VA (== PA) of the L0 table for the user window. M33's
    /// `SYS_MAP_NODE` installs new leaf PTEs here on demand.
    l0_pa: usize,
    /// Graph-node ids of `NodeType::Memory` nodes this task allocated
    /// via `SYS_MAP_NODE`. Cleaned up when the task exits — the nodes
    /// are removed from the graph (dropping the task→mem `write` edge
    /// with them). Backing frames follow the same lifetime as every
    /// other user frame the kernel allocates today (i.e. they stay
    /// resident; a frame-level free is a pre-existing M29 limitation).
    mem_node_ids: Vec<u64>,
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
///   maps them at USER_CODE_BASE / USER_DATA_BASE with R+W+X (exec) /
///   R (read) / R+W (write), all with PTE_U.
/// - Allocates a fresh stack frame at USER_STACK_BASE (R/W/U).
///
/// Exec edges span as many consecutive pages as the content needs (up
/// to USER_CODE_MAX_PAGES), so a real linker-placed Rust binary sees
/// one contiguous image at USER_CODE_BASE. See the exec-mapping loop
/// for the W^X trade-off that M31 accepts.
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
    //
    // Each exec edge maps as many 4 KiB pages as its content needs
    // (ceil(len/4096), minimum 1). Pages are consecutive in user VA, so
    // a real linker-placed binary (e.g. `hello-user`) sees one
    // contiguous image at USER_CODE_BASE. Asm demos with ≤ 4 KiB content
    // just get a single page and look the same as before.
    //
    // Flags are R+W+X+U for M31: a linker-placed binary needs .data
    // writable, and emitting two separate edges (one R+X for .text /
    // .rodata, one R+W for .data / .bss) would require the linker to
    // know exactly where the split lives. We give up strict W^X at the
    // task level until a later milestone splits images into `text` and
    // `rwdata` edges; capability enforcement across *tasks* is
    // unaffected (no edge → no mapping → no access).
    let mut code_slot = 0usize;
    let mut entry: Option<usize> = None;
    'outer: for &tgt_id in exec_targets.iter() {
        let tgt = match g.get_node(tgt_id) {
            Some(n) => n,
            None => continue,
        };
        let content = &tgt.content;
        let n_pages = core::cmp::max(1, (content.len() + 4095) / 4096);
        for page_i in 0..n_pages {
            if code_slot >= USER_CODE_MAX_PAGES {
                break 'outer;
            }
            let frame_pa = alloc_user_frame();
            let off = page_i * 4096;
            let copy_len = core::cmp::min(
                content.len().saturating_sub(off),
                4096,
            );
            if copy_len > 0 {
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        content.as_ptr().add(off),
                        frame_pa as *mut u8,
                        copy_len,
                    );
                }
            }
            let flags = PTE_R | PTE_W | PTE_X | PTE_U;
            l0.entries[code_slot] =
                PageTableEntry::leaf((frame_pa >> 12) as u64, flags);
            let va = USER_CODE_BASE + code_slot * 4096;
            mappings.push(Mapping { node_id: tgt_id, va, pa: frame_pa, kind: "exec" });
            if entry.is_none() {
                entry = Some(va);
            }
            code_slot += 1;
        }
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
    let l0_pa = l0 as *const _ as usize;

    Ok(UserAddressSpace {
        satp,
        mappings,
        entry: entry.ok_or("no entry point after mapping")?,
        l0_pa,
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
    let (exec_allowed, read_allowed, write_allowed, traverse_allowed) = {
        let g = graph::get();
        let task = g.get_node(task_node_id).expect("task node vanished");
        let mut exec = Vec::new();
        let mut read = Vec::new();
        let mut write = Vec::new();
        let mut traverse = Vec::new();
        for e in &task.edges {
            match e.label.as_str() {
                "exec" => exec.push(e.target),
                "read" => read.push(e.target),
                "write" => { read.push(e.target); write.push(e.target); }
                "traverse" => traverse.push(e.target),
                _ => {}
            }
        }
        (exec, read, write, traverse)
    };

    // Prepare the setjmp context and install ActiveUserTask.
    let mut kctx = KernelCtx::zero();
    kctx.satp = arch::read_satp();
    let kctx_ptr: *mut KernelCtx = &mut kctx;

    unsafe {
        ACTIVE = Some(ActiveUserTask {
            task_node_id,
            read_allowed,
            write_allowed,
            exec_allowed,
            traverse_allowed,
            kctx: kctx_ptr,
            exit_code: 0,
            faulted: false,
            l0_pa: aspace.l0_pa,
            mem_node_ids: Vec::new(),
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
    let (code, faulted, mem_nodes) = {
        let a = active().unwrap();
        (a.exit_code, a.faulted, a.mem_node_ids.clone())
    };
    unsafe { ACTIVE = None; }

    // M33: drop any memory nodes the task allocated via SYS_MAP_NODE.
    // `remove_node` also strips the task→mem `write` edge from the task
    // node, keeping the graph tidy. Backing frames share the pre-M33
    // per-task leak path (see the ActiveUserTask docstring).
    if !mem_nodes.is_empty() {
        let g = graph::get_mut();
        for id in &mem_nodes {
            g.remove_node(*id);
        }
    }

    // Mark the task node done.
    {
        let g = graph::get_mut();
        if let Some(node) = g.get_node_mut(task_node_id) {
            let info = alloc::format!(
                "user task (M29)\nexit: {}\nfaulted: {}\nmap_node count: {}\n",
                code, faulted, mem_nodes.len(),
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

/// Run a user task with an arbitrary set of capability edges.
///
/// `extra_edges` is a list of `(label, target_node_id)` pairs that get
/// added as outgoing edges from the synthesized task node. If
/// `self_traverse` is true, a `traverse` edge from the task back to
/// itself is added (so the task can enumerate its own edges via
/// SYS_LIST_EDGES / SYS_FOLLOW_EDGE).
///
/// `arg0`/`arg1` are the values placed in U-mode `a0`/`a1` at entry.
///
/// Returns the exit code (or negative value on fault).
pub fn run_user_task_with_caps(
    code_node_id: u64,
    extra_edges: &[(&str, u64)],
    self_traverse: bool,
    arg0: usize,
    arg1: usize,
) -> i64 {
    // Create the task node and wire up caps.
    let task_node_id = {
        let g = graph::get_mut();
        let n = USER_TASK_COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = alloc::format!("user-task-{}", n);
        let id = g.create_node(NodeType::System, &name);
        g.add_edge(1, "child", id);

        if !g.add_edge(id, "exec", code_node_id) {
            crate::println!("[user] couldn't add exec edge to node {}", code_node_id);
            return -1;
        }
        for (label, tgt) in extra_edges {
            if !g.add_edge(id, label, *tgt) {
                crate::println!("[user] couldn't add {} edge to node {}", label, tgt);
            }
        }
        if self_traverse {
            g.add_edge(id, "traverse", id);
        }

        if let Some(node) = g.get_node_mut(id) {
            let mut info = alloc::format!(
                "user task (M30)\nexec: {}\n", code_node_id
            );
            for (label, tgt) in extra_edges {
                info.push_str(&alloc::format!("{}: {}\n", label, tgt));
            }
            if self_traverse {
                info.push_str(&alloc::format!("traverse: {} (self)\n", id));
            }
            node.content = info.into_bytes();
        }
        id
    };

    run_user_task_inner(task_node_id, arg0, arg1)
}

/// Inner runner: build page table, drop to U-mode, collect result.
fn run_user_task_inner(task_node_id: u64, arg0: usize, arg1: usize) -> i64 {
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
    let (exec_allowed, read_allowed, write_allowed, traverse_allowed) = {
        let g = graph::get();
        let task = g.get_node(task_node_id).expect("task node vanished");
        let mut exec = Vec::new();
        let mut read = Vec::new();
        let mut write = Vec::new();
        let mut traverse = Vec::new();
        for e in &task.edges {
            match e.label.as_str() {
                "exec" => exec.push(e.target),
                "read" => read.push(e.target),
                "write" => { read.push(e.target); write.push(e.target); }
                "traverse" => traverse.push(e.target),
                _ => {}
            }
        }
        (exec, read, write, traverse)
    };

    let mut kctx = KernelCtx::zero();
    kctx.satp = arch::read_satp();
    let kctx_ptr: *mut KernelCtx = &mut kctx;

    unsafe {
        ACTIVE = Some(ActiveUserTask {
            task_node_id,
            read_allowed,
            write_allowed,
            exec_allowed,
            traverse_allowed,
            kctx: kctx_ptr,
            exit_code: 0,
            faulted: false,
            l0_pa: aspace.l0_pa,
            mem_node_ids: Vec::new(),
        });
    }

    let jmp = unsafe { user_setjmp(kctx_ptr) };
    if jmp == 0 {
        unsafe {
            let cur_sp = current_sp();
            (*kctx_ptr).satp = arch::read_satp();
            enter_usermode_asm(
                aspace.satp,
                aspace.entry,
                USER_STACK_TOP,
                arg0,
                arg1,
                cur_sp,
            );
        }
    }

    let (code, faulted, mem_nodes) = {
        let a = active().unwrap();
        (a.exit_code, a.faulted, a.mem_node_ids.clone())
    };
    unsafe { ACTIVE = None; }

    // M33: drop any SYS_MAP_NODE-allocated memory nodes. See the
    // matching block in `run_user_task_from_code_node` for rationale.
    if !mem_nodes.is_empty() {
        let g = graph::get_mut();
        for id in &mem_nodes {
            g.remove_node(*id);
        }
    }

    {
        let g = graph::get_mut();
        if let Some(node) = g.get_node_mut(task_node_id) {
            let info = alloc::format!(
                "user task\nexit: {}\nfaulted: {}\nmap_node count: {}\n",
                code, faulted, mem_nodes.len(),
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
        SYS_WRITE_NODE => {
            let node_id = frame.a0() as u64;
            let buf_va = frame.a1();
            let buf_len = frame.a2();
            let r = sys_write_node(node_id, buf_va, buf_len);
            frame.set_a0(r as usize);
        }
        SYS_LIST_EDGES => {
            let src = frame.a0() as u64;
            let buf_va = frame.a1();
            let max_entries = frame.a2();
            let r = sys_list_edges(src, buf_va, max_entries);
            frame.set_a0(r as usize);
        }
        SYS_FOLLOW_EDGE => {
            let src = frame.a0() as u64;
            let label_va = frame.a1();
            let label_len = frame.a2();
            let r = sys_follow_edge(src, label_va, label_len);
            frame.set_a0(r as usize);
        }
        SYS_SELF => {
            let id = active().map(|a| a.task_node_id).unwrap_or(0);
            frame.set_a0(id as usize);
        }
        SYS_MAP_NODE => {
            let size = frame.a0();
            let flags = frame.a1();
            let r = sys_map_node(size, flags);
            frame.set_a0(r as usize);
        }
        SYS_READ_EDGE_LABEL => {
            let src = frame.a0() as u64;
            let edge_index = frame.a1();
            let buf_va = frame.a2();
            let buf_len = frame.a3();
            let r = sys_read_edge_label(src, edge_index, buf_va, buf_len);
            frame.set_a0(r as usize);
        }
        _ => {
            crate::println!("[user] unknown syscall #{}", nr);
            frame.set_a0(EINVAL as usize);
        }
    }
}

// ---------------------------------------------------------------------------
// Capability-check helpers
// ---------------------------------------------------------------------------

/// Return true if the currently active task has an outgoing edge to
/// `target` labelled `label`. Centralises the cap check used by every
/// syscall in the M30 ABI.
fn has_cap(target: u64, label: &str) -> bool {
    active()
        .map(|a| match label {
            "read" => a.read_allowed.iter().any(|&t| t == target),
            "write" => a.write_allowed.iter().any(|&t| t == target),
            "traverse" => a.traverse_allowed.iter().any(|&t| t == target),
            "exec" => a.exec_allowed.iter().any(|&t| t == target),
            _ => false,
        })
        .unwrap_or(false)
}

/// Check that `[va, va+len)` lies strictly within the user VA window
/// (`USER_CODE_BASE..USER_STACK_TOP`). This prevents a U-mode task from
/// asking the kernel to copy bytes from/to unmapped or kernel VAs.
fn user_buf_ok(va: usize, len: usize) -> bool {
    if len == 0 {
        return true;
    }
    let end = match va.checked_add(len) {
        Some(e) => e,
        None => return false,
    };
    va >= USER_CODE_BASE && end <= USER_STACK_TOP
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
// M30: SYS_WRITE_NODE — overwrite a node's content.
// ---------------------------------------------------------------------------

fn sys_write_node(node_id: u64, buf_va: usize, buf_len: usize) -> i64 {
    crate::println!(
        "[sys] SYS_WRITE_NODE(node={}, buf_va={:#x}, len={})",
        node_id, buf_va, buf_len,
    );

    if !has_cap(node_id, "write") {
        let task_id = active().map(|a| a.task_node_id).unwrap_or(0);
        crate::println!(
            "[user] capability violation: task #{} tried to WRITE node {} (no `write` edge)",
            task_id, node_id
        );
        return EPERM;
    }

    // Cap to something sane.
    let n = core::cmp::min(buf_len, 4096);
    if !user_buf_ok(buf_va, n) {
        return EINVAL;
    }

    // Snapshot user bytes into a kernel-owned Vec before we borrow the
    // graph mutably.
    let mut bytes: alloc::vec::Vec<u8> = alloc::vec::Vec::with_capacity(n);
    unsafe {
        let p = buf_va as *const u8;
        for i in 0..n {
            bytes.push(core::ptr::read_volatile(p.add(i)));
        }
    }

    let g = graph::get_mut();
    let node = match g.get_node_mut(node_id) {
        Some(n) => n,
        None => return ENOENT,
    };
    node.content = bytes;
    n as i64
}

// ---------------------------------------------------------------------------
// M30: SYS_LIST_EDGES — enumerate outgoing edges from a node.
//
// Entry layout (16 bytes):
//     u64  target_id
//     u8   label_kind (1=read, 2=write, 3=exec, 4=traverse, 0=unknown)
//     u8[7] padding
// ---------------------------------------------------------------------------

fn sys_list_edges(src: u64, buf_va: usize, max_entries: usize) -> i64 {
    crate::println!(
        "[sys] SYS_LIST_EDGES(src={}, buf_va={:#x}, max={})",
        src, buf_va, max_entries,
    );

    if !has_cap(src, "traverse") {
        let task_id = active().map(|a| a.task_node_id).unwrap_or(0);
        crate::println!(
            "[user] capability violation: task #{} tried to LIST node {} (no `traverse` edge)",
            task_id, src
        );
        return EPERM;
    }

    let total_bytes = match max_entries.checked_mul(16) {
        Some(b) => b,
        None => return EINVAL,
    };
    if !user_buf_ok(buf_va, total_bytes) {
        return EINVAL;
    }

    // Snapshot the first N edges into a kernel-side vector of (target, kind)
    // tuples.
    let entries: alloc::vec::Vec<(u64, u8)> = {
        let g = graph::get();
        let node = match g.get_node(src) {
            Some(n) => n,
            None => return ENOENT,
        };
        node.edges
            .iter()
            .take(max_entries)
            .map(|e| (e.target, label_to_kind(&e.label)))
            .collect()
    };

    // Write entries into user memory.
    unsafe {
        let p = buf_va as *mut u8;
        for (i, (tgt, kind)) in entries.iter().enumerate() {
            let base = p.add(i * 16);
            // target_id as little-endian u64
            let tgt_bytes = tgt.to_le_bytes();
            for j in 0..8 {
                core::ptr::write_volatile(base.add(j), tgt_bytes[j]);
            }
            core::ptr::write_volatile(base.add(8), *kind);
            for j in 9..16 {
                core::ptr::write_volatile(base.add(j), 0);
            }
        }
    }

    entries.len() as i64
}

// ---------------------------------------------------------------------------
// M34: SYS_READ_EDGE_LABEL — read an edge's full string label by index.
//
// SYS_LIST_EDGES only returns the cap-kind byte per edge (read/write/
// exec/traverse/unknown). Structural edges like `child`/`parent`/`self`
// all come back as Unknown, so `spawn ls 1` prints `?` for all 18 root
// outgoing edges even though the graph holds meaningful label strings.
//
// This syscall closes that gap: given a source node id and the 0-based
// index of one of its outgoing edges (same ordering as LIST_EDGES, which
// is the graph's `Vec<Edge>` insertion order), copy the label's UTF-8
// bytes into a user buffer and return the byte count. No NUL
// termination — helios-std is responsible for interpreting the bytes
// as a str.
//
// Cap check: `traverse` from caller → src, exactly the same cap
// LIST_EDGES requires. This keeps the cap surface minimum: "if you
// could see the edge's target + kind, you can see its label".
//
// Failure modes and errno:
//   - no `traverse` edge                   → -EPERM
//   - user buf fails bounds check          → -EINVAL
//   - src node doesn't exist               → -ENOENT
//   - edge_index >= node.edges.len()       → -ENOENT
//   - buf_len < label.len()                → -EINVAL
//     (caller can retry with a bigger buffer)
// ---------------------------------------------------------------------------

fn sys_read_edge_label(
    src: u64,
    edge_index: usize,
    buf_va: usize,
    buf_len: usize,
) -> i64 {
    crate::println!(
        "[sys] SYS_READ_EDGE_LABEL(src={}, idx={}, buf_va={:#x}, len={})",
        src, edge_index, buf_va, buf_len,
    );

    if !has_cap(src, "traverse") {
        let task_id = active().map(|a| a.task_node_id).unwrap_or(0);
        crate::println!(
            "[user] capability violation: task #{} tried to READ_EDGE_LABEL on node {} (no `traverse` edge)",
            task_id, src
        );
        return EPERM;
    }

    if !user_buf_ok(buf_va, buf_len) {
        return EINVAL;
    }

    // Snapshot the label bytes under the graph borrow, then release
    // before touching user memory.
    let label_bytes: alloc::vec::Vec<u8> = {
        let g = graph::get();
        let node = match g.get_node(src) {
            Some(n) => n,
            None => return ENOENT,
        };
        let edge = match node.edges.get(edge_index) {
            Some(e) => e,
            None => return ENOENT,
        };
        edge.label.as_bytes().to_vec()
    };

    if buf_len < label_bytes.len() {
        // Caller's buffer is too small to hold the full label. Tell them
        // so they can retry with a larger buffer; don't truncate.
        return EINVAL;
    }

    // SUM is set, bounds are verified: direct write into user memory.
    unsafe {
        let p = buf_va as *mut u8;
        for (i, b) in label_bytes.iter().enumerate() {
            core::ptr::write_volatile(p.add(i), *b);
        }
    }

    label_bytes.len() as i64
}

// ---------------------------------------------------------------------------
// M33: SYS_MAP_NODE — kernel-granted anonymous writable memory.
//
// The user asks for N bytes of zeroed writable memory. We round up to
// a 4 KiB multiple, allocate that many frames, create a fresh
// `NodeType::Memory` graph node to own them, add a `write` edge from
// the caller's task node to the new memory node (which auto-implies
// `read` under the M30 cap semantics), and map the frames into the
// caller's data-VA window as R+W+U leaves. On success we return the
// user VA of the first mapped page; on failure we return a negative
// errno.
//
// VA window management. The task's data window is 16 4 KiB slots at
// `USER_DATA_BASE..USER_DATA_BASE+USER_DATA_MAX_PAGES*4096`. On each
// call we walk the L0 table in that range looking for a contiguous
// run of `n_pages` unused slots. This is the bitmap-over-16-slots
// approach from the M33 proposal (docs/design/proposals/post-m32-
// directions.md, Proposal A) — except we don't materialise a separate
// bitmap, we just inspect the V bit of each PTE directly. At 16 slots
// the walk is trivial; a denser scheme would be overkill.
//
// Failure modes and errno:
//   - `flags != 0`                        → -EINVAL
//   - `size == 0`                         → -EINVAL
//   - `size` rounded up > 16 * 4 KiB      → -EINVAL
//   - no contiguous run of free slots     → -ENOMEM
//   - no active user task                 → -EINVAL (shouldn't happen)
// ---------------------------------------------------------------------------

/// Counter for synthesised `user-mem-N` node names.
static MEM_NODE_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Find a contiguous run of `n` unused slots in the data-VA window
/// starting at `data_start`, returning the first slot's *offset within*
/// `[data_start..data_start+USER_DATA_MAX_PAGES]` (i.e. 0-based).
fn find_free_data_run(l0: &PageTable, data_start: usize, n: usize) -> Option<usize> {
    if n == 0 || n > USER_DATA_MAX_PAGES {
        return None;
    }
    let mut run_begin: Option<usize> = None;
    let mut run_len: usize = 0;
    for i in 0..USER_DATA_MAX_PAGES {
        let slot = data_start + i;
        let used = (l0.entries[slot].0 & PTE_V) != 0;
        if used {
            run_begin = None;
            run_len = 0;
        } else {
            if run_begin.is_none() {
                run_begin = Some(i);
            }
            run_len += 1;
            if run_len == n {
                return run_begin;
            }
        }
    }
    None
}

fn sys_map_node(size: usize, flags: usize) -> i64 {
    // Validate flags: only 0 is defined in M33.
    if flags != 0 {
        crate::println!("[sys] SYS_MAP_NODE: unsupported flags={:#x}", flags);
        return EINVAL;
    }
    if size == 0 {
        return EINVAL;
    }

    // Round up to 4 KiB multiple.
    let aligned = match size.checked_add(4095) {
        Some(v) => v & !4095usize,
        None => return EINVAL,
    };
    let max_bytes = USER_DATA_MAX_PAGES * 4096;
    if aligned > max_bytes {
        return EINVAL;
    }
    let n_pages = aligned / 4096;

    // Fetch the task's L0 table PA. `active()` is set for the duration
    // of a U-mode task (see run_user_task_inner); syscalls only fire
    // from U-mode, so this should always be `Some`.
    let (l0_pa, task_id) = match active() {
        Some(a) => (a.l0_pa, a.task_node_id),
        None => {
            crate::println!("[sys] SYS_MAP_NODE: no active user task");
            return EINVAL;
        }
    };
    // SAFETY: l0_pa was filled from a kernel-side `alloc_page_table()`
    // result when we built the address space, so it's a live,
    // identity-mapped 4 KiB-aligned `PageTable`.
    let l0 = unsafe { &mut *(l0_pa as *mut PageTable) };

    let data_start = (USER_DATA_BASE - USER_CODE_BASE) / 4096; // = 256
    let slot_off = match find_free_data_run(l0, data_start, n_pages) {
        Some(o) => o,
        None => {
            crate::println!(
                "[sys] SYS_MAP_NODE: no contiguous run of {} slot(s) in data window",
                n_pages,
            );
            return ENOMEM;
        }
    };

    // Allocate `n_pages` fresh zeroed frames up front. `alloc_user_frame`
    // pulls from the kernel heap, which is identity-mapped.
    let mut frames: Vec<usize> = Vec::with_capacity(n_pages);
    for _ in 0..n_pages {
        frames.push(alloc_user_frame());
    }

    // Create the graph node that owns those frames.
    let mem_id = {
        let g = graph::get_mut();
        let n = MEM_NODE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = alloc::format!("user-mem-{}", n);
        let id = g.create_node(NodeType::Memory, &name);
        // Add the task→mem `write` edge. Write implies read under the
        // M30 cap semantics, so the task can also `SYS_READ_NODE` this
        // node (not just touch its pages via MMU).
        g.add_edge(task_id, "write", id);
        // Stash a small diagnostic string as the node's content so
        // SYS_LIST_EDGES / navigator views show something meaningful;
        // the user-visible mapping is the frames themselves, not this.
        if let Some(node) = g.get_node_mut(id) {
            node.content = alloc::format!(
                "anon mem: size={} bytes ({} page(s)), va={:#x}\n",
                aligned,
                n_pages,
                USER_DATA_BASE + slot_off * 4096,
            )
            .into_bytes();
        }
        id
    };

    // Install the leaf PTEs. Each frame gets R+W+U (readable and
    // writable from U-mode; invisible to other tasks — no edge, no
    // mapping, no access).
    let leaf_flags = PTE_R | PTE_W | PTE_U;
    for (i, &pa) in frames.iter().enumerate() {
        l0.entries[data_start + slot_off + i] =
            PageTableEntry::leaf((pa >> 12) as u64, leaf_flags);
    }

    // Update the active task's cap snapshots (so syscalls targeting
    // the new node see it as allowed too) and track the node for
    // cleanup on exit.
    if let Some(a) = active_mut() {
        a.read_allowed.push(mem_id);
        a.write_allowed.push(mem_id);
        a.mem_node_ids.push(mem_id);
    }

    // Flush the TLB for the current asid. Strictly, an invalid→valid
    // PTE transition doesn't require a fence on RISC-V, but being
    // conservative here costs essentially nothing.
    unsafe { core::arch::asm!("sfence.vma zero, zero"); }

    let va = USER_DATA_BASE + slot_off * 4096;
    crate::println!(
        "[sys] SYS_MAP_NODE(size={}, flags={}) -> node #{}, va={:#010x}, {} page(s)",
        size, flags, mem_id, va, n_pages,
    );
    va as i64
}

// ---------------------------------------------------------------------------
// M30: SYS_FOLLOW_EDGE — find the first outgoing edge from `src` whose
// label exactly matches the given string, and return the target node id.
// ---------------------------------------------------------------------------

fn sys_follow_edge(src: u64, label_va: usize, label_len: usize) -> i64 {
    if label_len == 0 || label_len > 64 {
        return EINVAL;
    }
    if !user_buf_ok(label_va, label_len) {
        return EINVAL;
    }
    if !has_cap(src, "traverse") {
        let task_id = active().map(|a| a.task_node_id).unwrap_or(0);
        crate::println!(
            "[user] capability violation: task #{} tried to FOLLOW from node {} (no `traverse` edge)",
            task_id, src
        );
        return EPERM;
    }

    // Snapshot the label bytes into a kernel buffer.
    let mut lbuf: [u8; 64] = [0; 64];
    unsafe {
        let p = label_va as *const u8;
        for i in 0..label_len {
            lbuf[i] = core::ptr::read_volatile(p.add(i));
        }
    }
    let label = match core::str::from_utf8(&lbuf[..label_len]) {
        Ok(s) => s,
        Err(_) => return EINVAL,
    };

    crate::println!(
        "[sys] SYS_FOLLOW_EDGE(src={}, label=\"{}\")", src, label,
    );

    let g = graph::get();
    let node = match g.get_node(src) {
        Some(n) => n,
        None => return ENOENT,
    };
    for e in &node.edges {
        if e.label == label {
            return e.target as i64;
        }
    }
    ENOENT
}

// ---------------------------------------------------------------------------
// Boot-time demo graph setup
// ---------------------------------------------------------------------------

/// Demo node ids, populated by `init()`.
static mut DEMO_CODE_ID: u64 = 0;
static mut BADDEMO_CODE_ID: u64 = 0;
static mut DEMO_TEXT_ID: u64 = 0;
static mut WHO_CODE_ID: u64 = 0;
static mut EXPLORER_CODE_ID: u64 = 0;
static mut EDITOR_CODE_ID: u64 = 0;
static mut NAUGHTY_CODE_ID: u64 = 0;
static mut SCRATCH_ID: u64 = 0;
/// M31: node id of the `hello` Rust-native user program.
static mut HELLO_CODE_ID: u64 = 0;
/// M32: node id of the `ls` graph-native Rust-native user program.
static mut LS_CODE_ID: u64 = 0;
/// M32: node id of the `cat` graph-native Rust-native user program.
static mut CAT_CODE_ID: u64 = 0;
/// M33: node id of the `mmap` demo (exercises SYS_MAP_NODE).
static mut MMAP_CODE_ID: u64 = 0;
/// M33.5: node id of the `bigalloc` demo (GlobalAlloc via SYS_MAP_NODE).
static mut BIGALLOC_CODE_ID: u64 = 0;

/// Initialize the demo user-space nodes: a Binary code node for each
/// demo + a Text node the M29 demo reads + the scratch node the M30
/// editor/naughty demos target.
#[allow(static_mut_refs)]
pub fn init() {
    let g = graph::get_mut();

    // M29 demos (keep working).
    let bytes = demo_program_bytes();
    let bad_bytes = baddemo_program_bytes();

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

    // M30 demos.
    let who_bytes = who_program_bytes();
    let explorer_bytes = explorer_program_bytes();
    let editor_bytes = editor_program_bytes();
    let naughty_bytes = naughty_program_bytes();

    let who_id = g.create_node(NodeType::Binary, "user-who-code");
    if let Some(n) = g.get_node_mut(who_id) { n.content = who_bytes.to_vec(); }
    g.add_edge(1, "child", who_id);

    let exp_id = g.create_node(NodeType::Binary, "user-explorer-code");
    if let Some(n) = g.get_node_mut(exp_id) { n.content = explorer_bytes.to_vec(); }
    g.add_edge(1, "child", exp_id);

    let ed_id = g.create_node(NodeType::Binary, "user-editor-code");
    if let Some(n) = g.get_node_mut(ed_id) { n.content = editor_bytes.to_vec(); }
    g.add_edge(1, "child", ed_id);

    let nau_id = g.create_node(NodeType::Binary, "user-naughty-code");
    if let Some(n) = g.get_node_mut(nau_id) { n.content = naughty_bytes.to_vec(); }
    g.add_edge(1, "child", nau_id);

    let scratch_id = g.create_node(NodeType::Text, "user-scratch");
    if let Some(n) = g.get_node_mut(scratch_id) {
        n.content = b"initial scratch content.\n".to_vec();
    }
    g.add_edge(1, "child", scratch_id);

    // M31: Rust-native hello-world linked against helios-std.
    let hello_bytes = hello_program_bytes();
    let hello_id = g.create_node(NodeType::Binary, "hello-user-code");
    if let Some(n) = g.get_node_mut(hello_id) {
        n.content = hello_bytes.to_vec();
    }
    g.add_edge(1, "child", hello_id);

    // M32: graph-native `ls` + `cat` user programs, linked against
    // helios-std. Both treat a node id as their only input (via a0).
    let ls_bytes = ls_program_bytes();
    let ls_id = g.create_node(NodeType::Binary, "ls-user-code");
    if let Some(n) = g.get_node_mut(ls_id) { n.content = ls_bytes.to_vec(); }
    g.add_edge(1, "child", ls_id);

    let cat_bytes = cat_program_bytes();
    let cat_id = g.create_node(NodeType::Binary, "cat-user-code");
    if let Some(n) = g.get_node_mut(cat_id) { n.content = cat_bytes.to_vec(); }
    g.add_edge(1, "child", cat_id);

    // M33: SYS_MAP_NODE demo program.
    let mmap_bytes = mmap_program_bytes();
    let mmap_id = g.create_node(NodeType::Binary, "mmap-user-code");
    if let Some(n) = g.get_node_mut(mmap_id) { n.content = mmap_bytes.to_vec(); }
    g.add_edge(1, "child", mmap_id);

    // M33.5: GlobalAlloc-via-SYS_MAP_NODE smoke test.
    let bigalloc_bytes = bigalloc_program_bytes();
    let bigalloc_id = g.create_node(NodeType::Binary, "bigalloc-user-code");
    if let Some(n) = g.get_node_mut(bigalloc_id) { n.content = bigalloc_bytes.to_vec(); }
    g.add_edge(1, "child", bigalloc_id);

    unsafe {
        DEMO_CODE_ID = code_id;
        BADDEMO_CODE_ID = bad_id;
        DEMO_TEXT_ID = text_id;
        WHO_CODE_ID = who_id;
        EXPLORER_CODE_ID = exp_id;
        EDITOR_CODE_ID = ed_id;
        NAUGHTY_CODE_ID = nau_id;
        SCRATCH_ID = scratch_id;
        HELLO_CODE_ID = hello_id;
        LS_CODE_ID = ls_id;
        CAT_CODE_ID = cat_id;
        MMAP_CODE_ID = mmap_id;
        BIGALLOC_CODE_ID = bigalloc_id;
    }
    crate::println!(
        "[user] demo nodes ready: demo=#{} ({}B) bad=#{} ({}B) text=#{}",
        code_id, bytes.len(), bad_id, bad_bytes.len(), text_id,
    );
    crate::println!(
        "[user] M30 demos: who=#{} ({}B) explorer=#{} ({}B) editor=#{} ({}B) naughty=#{} ({}B) scratch=#{}",
        who_id, who_bytes.len(), exp_id, explorer_bytes.len(),
        ed_id, editor_bytes.len(), nau_id, naughty_bytes.len(), scratch_id,
    );
    crate::println!(
        "[user] M31 native Rust: hello=#{} ({} B)",
        hello_id, hello_bytes.len(),
    );
    crate::println!(
        "[user] M32 native Rust: ls=#{} ({} B) cat=#{} ({} B)",
        ls_id, ls_bytes.len(), cat_id, cat_bytes.len(),
    );
    crate::println!(
        "[user] M33 native Rust: mmap=#{} ({} B)",
        mmap_id, mmap_bytes.len(),
    );
    crate::println!(
        "[user] M33.5 native Rust: bigalloc=#{} ({} B)",
        bigalloc_id, bigalloc_bytes.len(),
    );
}

#[allow(static_mut_refs)]
pub fn demo_code_id() -> u64 { unsafe { DEMO_CODE_ID } }
#[allow(static_mut_refs)]
pub fn baddemo_code_id() -> u64 { unsafe { BADDEMO_CODE_ID } }
#[allow(static_mut_refs)]
pub fn demo_text_id() -> u64 { unsafe { DEMO_TEXT_ID } }
#[allow(static_mut_refs)]
pub fn who_code_id() -> u64 { unsafe { WHO_CODE_ID } }
#[allow(static_mut_refs)]
pub fn explorer_code_id() -> u64 { unsafe { EXPLORER_CODE_ID } }
#[allow(static_mut_refs)]
pub fn editor_code_id() -> u64 { unsafe { EDITOR_CODE_ID } }
#[allow(static_mut_refs)]
pub fn naughty_code_id() -> u64 { unsafe { NAUGHTY_CODE_ID } }
#[allow(static_mut_refs)]
pub fn scratch_id() -> u64 { unsafe { SCRATCH_ID } }
/// Node id of the compiled `hello-user` Rust binary (M31).
#[allow(static_mut_refs)]
pub fn hello_code_id() -> u64 { unsafe { HELLO_CODE_ID } }
/// Node id of the compiled `ls-user` Rust binary (M32).
#[allow(static_mut_refs)]
pub fn ls_code_id() -> u64 { unsafe { LS_CODE_ID } }
/// Node id of the compiled `cat-user` Rust binary (M32).
#[allow(static_mut_refs)]
pub fn cat_code_id() -> u64 { unsafe { CAT_CODE_ID } }
/// Node id of the compiled `mmap-user` Rust binary (M33).
#[allow(static_mut_refs)]
pub fn mmap_code_id() -> u64 { unsafe { MMAP_CODE_ID } }
/// Node id of the compiled `bigalloc-user` Rust binary (M33.5).
#[allow(static_mut_refs)]
pub fn bigalloc_code_id() -> u64 { unsafe { BIGALLOC_CODE_ID } }
