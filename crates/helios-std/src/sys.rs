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
//!
//! # Host-target compilation
//!
//! All `ecall`-emitting bodies are gated behind
//! `#[cfg(target_arch = "riscv64")]`. On any other target (host
//! compilation for unit tests, IDE checks, doc builds) the same
//! function signatures are kept but their bodies become
//! `unimplemented!()`. This lets `helios-std`'s pure-data modules
//! (`graph::Label`, `graph::Errno`, edge serialization, etc.) be
//! exercised by `cargo test` on the developer's host triple. The
//! build-std + riscv64 path is unaffected — every real Helios user
//! binary still gets the inline-asm `ecall` bodies.

#[cfg(target_arch = "riscv64")]
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
/// M35: copy one of the caller's outgoing edges onto another task,
/// recording the caller's edge as the new edge's CDT parent. Needs both
/// the matching outgoing edge AND a live `grant` edge on the target.
pub const SYS_DELEGATE_EDGE: usize = 10;
/// M35: tombstone one of the caller's outgoing edges and every CDT
/// descendant. Cascades transitively. For exec/read/write edges on the
/// active task, the corresponding PT slots are unmapped + TLB flushed.
pub const SYS_REVOKE_EDGE: usize = 11;

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
#[cfg(target_arch = "riscv64")]
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

/// Host stub for `syscall0` — see module-level docs. The signature is
/// preserved so callers compile on host; calling at runtime panics.
#[cfg(not(target_arch = "riscv64"))]
#[inline(always)]
pub unsafe fn syscall0(_nr: usize) -> isize {
    unimplemented!("syscall0 is only available on riscv64 (Helios target)")
}

/// Invoke a syscall with one argument.
#[cfg(target_arch = "riscv64")]
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

/// Host stub for `syscall1`.
#[cfg(not(target_arch = "riscv64"))]
#[inline(always)]
pub unsafe fn syscall1(_nr: usize, _a0: usize) -> isize {
    unimplemented!("syscall1 is only available on riscv64 (Helios target)")
}

/// Invoke a syscall with two arguments.
#[cfg(target_arch = "riscv64")]
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

/// Host stub for `syscall2`.
#[cfg(not(target_arch = "riscv64"))]
#[inline(always)]
pub unsafe fn syscall2(_nr: usize, _a0: usize, _a1: usize) -> isize {
    unimplemented!("syscall2 is only available on riscv64 (Helios target)")
}

/// Invoke a syscall with three arguments.
#[cfg(target_arch = "riscv64")]
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

/// Host stub for `syscall3`.
#[cfg(not(target_arch = "riscv64"))]
#[inline(always)]
pub unsafe fn syscall3(_nr: usize, _a0: usize, _a1: usize, _a2: usize) -> isize {
    unimplemented!("syscall3 is only available on riscv64 (Helios target)")
}

/// Invoke a syscall with four arguments.
#[cfg(target_arch = "riscv64")]
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

/// Host stub for `syscall4`.
#[cfg(not(target_arch = "riscv64"))]
#[inline(always)]
pub unsafe fn syscall4(
    _nr: usize,
    _a0: usize,
    _a1: usize,
    _a2: usize,
    _a3: usize,
) -> isize {
    unimplemented!("syscall4 is only available on riscv64 (Helios target)")
}

/// `SYS_EXIT(code)` — does not return.
#[cfg(target_arch = "riscv64")]
#[inline(always)]
pub unsafe fn syscall_exit(code: i32) -> ! {
    asm!(
        "ecall",
        in("a7") SYS_EXIT,
        in("a0") code as usize,
        options(noreturn),
    );
}

/// Host stub for `syscall_exit`. Aborts the host-side test process so
/// the `-> !` signature is honored without inline asm.
#[cfg(not(target_arch = "riscv64"))]
#[inline(always)]
pub unsafe fn syscall_exit(_code: i32) -> ! {
    unimplemented!("syscall_exit is only available on riscv64 (Helios target)")
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

/// `SYS_DELEGATE_EDGE(target_node_id, target_task_node_id, label_va,
/// label_len)` — copy one of the caller's outgoing edges onto another
/// task. The caller must hold a live outgoing edge with `label`
/// pointing at `target_node_id`, AND a live `grant` edge on
/// `target_node_id`. The new edge records the caller's edge as its CDT
/// parent — revoking the caller's edge cascades to remove this one.
///
/// Returns the raw vec_position of the new edge on success (a small
/// non-negative integer), or a negative errno on failure:
///
/// - `EPERM` — caller lacks the source edge OR lacks `grant` on target.
/// - `ENOENT` — `target_task_node_id` or `target_node_id` missing.
/// - `EINVAL` — bad label (empty / >64 bytes / not UTF-8 / out-of-range
///   buffer).
///
/// See [`crate::graph::delegate_edge`] for the typed wrapper.
#[inline(always)]
pub unsafe fn sys_delegate_edge(
    target_node_id: u64,
    target_task_node_id: u64,
    label_va: *const u8,
    label_len: usize,
) -> isize {
    syscall4(
        SYS_DELEGATE_EDGE,
        target_node_id as usize,
        target_task_node_id as usize,
        label_va as usize,
        label_len,
    )
}

/// `SYS_REVOKE_EDGE(target_node_id, label_va, label_len)` — tombstone
/// the caller's outgoing edge `(label, target_node_id)` and every CDT
/// descendant. For exec/read/write edges on the active task, the
/// corresponding PT slots are unmapped and the TLB is flushed.
///
/// Returns the count of tombstoned edges on success (>= 1), or a
/// negative errno on failure:
///
/// - `EINVAL` — bad label (empty / >64 bytes / not UTF-8 / out-of-range
///   buffer) OR no active task.
/// - `ENOENT` — no matching live outgoing edge on the caller.
///
/// See [`crate::graph::revoke_edge`] for the typed wrapper.
#[inline(always)]
pub unsafe fn sys_revoke_edge(
    target_node_id: u64,
    label_va: *const u8,
    label_len: usize,
) -> isize {
    syscall3(
        SYS_REVOKE_EDGE,
        target_node_id as usize,
        label_va as usize,
        label_len,
    )
}

// ---------------------------------------------------------------------------
// Host-side unit tests for syscall numbers + errno constants.
// ---------------------------------------------------------------------------
//
// These pin the ABI to fixed bytes. The kernel side has the matching
// constants in `src/user.rs`; if either drifts the userspace
// programs silently start invoking the wrong syscall (or
// misinterpreting return codes). No QEMU needed to catch that.

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the 11 syscall numbers (M30 through M35) to their kernel
    /// ABI values. If the kernel renumbers, this test fails before the
    /// next QEMU run does.
    #[test]
    fn syscall_numbers_match_kernel_abi() {
        assert_eq!(SYS_READ_NODE, 1);
        assert_eq!(SYS_PRINT, 2);
        assert_eq!(SYS_EXIT, 3);
        assert_eq!(SYS_WRITE_NODE, 4);
        assert_eq!(SYS_LIST_EDGES, 5);
        assert_eq!(SYS_FOLLOW_EDGE, 6);
        assert_eq!(SYS_SELF, 7);
        assert_eq!(SYS_MAP_NODE, 8);
        assert_eq!(SYS_READ_EDGE_LABEL, 9);
        assert_eq!(SYS_DELEGATE_EDGE, 10);
        assert_eq!(SYS_REVOKE_EDGE, 11);
    }

    /// Errno constants are negative single-digit values; pin them to
    /// stay in sync with `crate::graph::Errno::from_raw`.
    #[test]
    fn errno_constants_match_kernel_abi() {
        assert_eq!(EPERM, -1);
        assert_eq!(ENOENT, -2);
        assert_eq!(EINVAL, -3);
        assert_eq!(ENOMEM, -4);
    }
}
