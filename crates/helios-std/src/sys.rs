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
/// M33: kernel-granted anonymous writable memory.
pub const SYS_MAP_NODE: usize = 8;
/// M34: read an outgoing edge's full string label by index. Closes the
/// "everything shows as ?" gap in `SYS_LIST_EDGES`.
pub const SYS_READ_EDGE_LABEL: usize = 9;

// ---------------------------------------------------------------------------
// Errno values returned by syscalls (matching the kernel's constants)
// ---------------------------------------------------------------------------

pub const EPERM: isize = -1;
pub const ENOENT: isize = -2;
pub const EINVAL: isize = -3;
/// M33: out of memory (no backing frames or no contiguous VA slots).
pub const ENOMEM: isize = -4;

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

/// Invoke a syscall with four arguments.
#[inline(always)]
pub unsafe fn syscall4(nr: usize, a0: usize, a1: usize, a2: usize, a3: usize) -> isize {
    let ret: isize;
    asm!(
        "ecall",
        in("a7") nr,
        inlateout("a0") a0 => ret,
        in("a1") a1,
        in("a2") a2,
        in("a3") a3,
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

/// `SYS_MAP_NODE(size, flags)` — ask the kernel for `size` bytes of
/// fresh zeroed writable memory. Returns the user VA of the first
/// mapped page, or a negative errno.
///
/// Flags must be 0 in M33 (reserved for future use, e.g. "map at a
/// specific VA hint" or "never-reclaim"). See
/// [`crate::graph::map_node`] for the typed wrapper most call sites
/// should use instead.
#[inline(always)]
pub unsafe fn sys_map_node(size: usize, flags: usize) -> isize {
    syscall2(SYS_MAP_NODE, size, flags)
}

/// `SYS_READ_EDGE_LABEL(src_id, edge_index, buf, buf_len)` — copy the
/// full UTF-8 bytes of the edge's label into `buf`, return the byte
/// count. No NUL terminator; callers interpret the slice
/// `buf[..ret as usize]` as UTF-8.
///
/// Returns:
///
/// - positive bytes-written on success
/// - `EPERM` if the caller lacks a `traverse` edge to `src_id`
/// - `ENOENT` if `src_id` is missing or `edge_index` is out of range
/// - `EINVAL` if the user buffer is out-of-range OR too small for the
///   full label (retry with a bigger buffer in that case)
///
/// See [`crate::graph::read_edge_label`] for the typed wrapper that
/// handles the grow-retry + UTF-8 decode for you.
#[inline(always)]
pub unsafe fn sys_read_edge_label(
    src_id: u64,
    edge_index: usize,
    buf: *mut u8,
    buf_len: usize,
) -> isize {
    syscall4(
        SYS_READ_EDGE_LABEL,
        src_id as usize,
        edge_index,
        buf as usize,
        buf_len,
    )
}
