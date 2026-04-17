//! Raw syscall wrappers.
//!
//! Each `syscall_N` helper below is a direct `ecall` with the Helios
//! ABI: syscall number in `a7`, arguments in `a0`..`a6`, return in
//! `a0`. Return values are `isize` — negative values encode errno
//! (`-1` = EPERM, `-2` = ENOENT, `-3` = EINVAL); see [`crate::graph::Errno`]
//! for a typed view.
//!
//! Higher-level typed wrappers live in [`crate::graph`], [`crate::io`],
//! and [`crate::task`]. Reach for those first; `sys` is the
//! low-level escape hatch.

use core::arch::asm;

// ---------------------------------------------------------------------------
// Syscall numbers (must stay in sync with src/user.rs in the kernel)
// ---------------------------------------------------------------------------

pub const SYS_READ_NODE: usize = 1;
pub const SYS_PRINT: usize = 2;
pub const SYS_EXIT: usize = 3;
pub const SYS_WRITE_NODE: usize = 4;
pub const SYS_LIST_EDGES: usize = 5;
pub const SYS_FOLLOW_EDGE: usize = 6;
pub const SYS_SELF: usize = 7;

// ---------------------------------------------------------------------------
// Errno values returned by syscalls (matching the kernel's constants)
// ---------------------------------------------------------------------------

pub const EPERM: isize = -1;
pub const ENOENT: isize = -2;
pub const EINVAL: isize = -3;

// ---------------------------------------------------------------------------
// Raw `ecall` helpers
// ---------------------------------------------------------------------------

/// Invoke a syscall with no arguments. Returns the raw `a0` result.
#[inline(always)]
pub unsafe fn syscall0(nr: usize) -> isize {
    let ret: isize;
    asm!(
        "ecall",
        in("a7") nr,
        lateout("a0") ret,
        options(nostack, preserves_flags),
    );
    ret
}

/// Invoke a syscall with one argument.
#[inline(always)]
pub unsafe fn syscall1(nr: usize, a0: usize) -> isize {
    let ret: isize;
    asm!(
        "ecall",
        in("a7") nr,
        inlateout("a0") a0 => ret,
        options(nostack, preserves_flags),
    );
    ret
}

/// Invoke a syscall with two arguments.
#[inline(always)]
pub unsafe fn syscall2(nr: usize, a0: usize, a1: usize) -> isize {
    let ret: isize;
    asm!(
        "ecall",
        in("a7") nr,
        inlateout("a0") a0 => ret,
        in("a1") a1,
        options(nostack, preserves_flags),
    );
    ret
}

/// Invoke a syscall with three arguments.
#[inline(always)]
pub unsafe fn syscall3(nr: usize, a0: usize, a1: usize, a2: usize) -> isize {
    let ret: isize;
    asm!(
        "ecall",
        in("a7") nr,
        inlateout("a0") a0 => ret,
        in("a1") a1,
        in("a2") a2,
        options(nostack, preserves_flags),
    );
    ret
}

/// `SYS_EXIT(code)` — does not return.
#[inline(always)]
pub unsafe fn syscall_exit(code: i32) -> ! {
    asm!(
        "ecall",
        in("a7") SYS_EXIT,
        in("a0") code as usize,
        options(noreturn),
    );
}
