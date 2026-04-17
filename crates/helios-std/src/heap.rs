//! Slab-chained bump allocator backed by the kernel's `SYS_MAP_NODE`.
//!
//! # History
//!
//! M31 shipped a 64 KiB `[0xAA; N]` arena that lived *inside* the user
//! binary's `.data` section. Every Rust user program carried that 64 KiB
//! of padding on disk and was forever capped at 64 KiB of dynamic
//! memory. M33 added `SYS_MAP_NODE` (kernel-granted anonymous writable
//! memory) but did not yet reroute `GlobalAlloc` through it. M33.5 (this
//! file) closes that loop: the in-binary arena is gone; every
//! allocation that can't fit the current slab requests a fresh one from
//! the kernel.
//!
//! # Shape
//!
//! A single running "current slab" — base / end / cursor tracked in
//! `.data` — serves bump allocations until it runs out. On exhaustion,
//! the allocator calls [`crate::graph::map_node`] to obtain a new slab
//! (default 16 KiB; sized up to fit oversized requests) and installs it
//! as the current slab. Previous slabs are not tracked for reuse — bump
//! allocators don't free individual allocations, and old slabs stay live
//! only through whatever references Rust still holds into them. The
//! kernel's per-task `mem_node_ids` cleanup reclaims every slab when the
//! task exits (see `src/user.rs` → `cleanup_exited_user_task`).
//!
//! # Why no bootstrap arena?
//!
//! The `_start` shim expanded by [`crate::helios_entry`] does not
//! allocate: it stashes two `AtomicUsize` entry-args and calls `main`.
//! The panic handler writes via `core::write!` to a ZST
//! [`crate::io::Stdout`] and issues `SYS_EXIT` — also no alloc path.
//! So lazy init is sound: the first `alloc` call triggers the first
//! `map_node` syscall, and nothing before `main()` can hit the
//! allocator. Removing the bootstrap shaves ~64 KiB off every binary
//! and brings the on-disk footprint of `hello-user` down to the
//! program + stdlib-aware glue (sub-10 KiB).
//!
//! # Scope cuts (accepted for M33.5, documented here)
//!
//! - **No per-allocation free.** Bump-within-slab; slabs live until the
//!   task exits. A future "real allocator" milestone could add a
//!   free-list or seL4-style object-per-slab scheme.
//! - **No cross-task state.** Each user task has its own allocator with
//!   its own slabs.
//! - **No alignment-waste tracking.** [`used`] reports a
//!   best-effort high-water mark; exact accounting is out of scope.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::graph::map_node;

/// Default slab size requested from the kernel: 16 KiB.
///
/// Chosen so that up to four slabs can coexist in the task's 64 KiB
/// data window, giving real slab-chaining headroom. Raised freely if
/// the kernel ever widens the window.
pub const SLAB_DEFAULT: usize = 16 * 1024;

/// Hard upper bound on the number of slabs we'll install per task.
///
/// The kernel's 16-page data window means at most ~16 single-page
/// slabs, and in practice a `map_node(SLAB_DEFAULT)` slab is 4 pages,
/// so the real limit is ~4. We keep the counter just as an OOM
/// tripwire — if the user program somehow requests many tiny slabs,
/// we refuse politely rather than spin forever issuing syscalls.
pub const MAX_SLABS: usize = 16;

// Allocator state in `.data.helios_heap` so the whole footprint
// co-locates for auditing. Atomics are a single-instruction pattern
// that the compiler can't reorder across a syscall boundary — the
// previous UnsafeCell experiment showed LTO/inliner could cache
// stale reads across a nested call that didn't touch the same
// visible memory, even when logically it did. Atomics settle the
// codegen.
#[link_section = ".data.helios_heap"]
static CURRENT_BASE: AtomicUsize = AtomicUsize::new(0);

#[link_section = ".data.helios_heap"]
static CURRENT_END: AtomicUsize = AtomicUsize::new(0);

#[link_section = ".data.helios_heap"]
static CURRENT_CURSOR: AtomicUsize = AtomicUsize::new(0);

#[link_section = ".data.helios_heap"]
static SLAB_COUNT: AtomicUsize = AtomicUsize::new(0);

#[link_section = ".data.helios_heap"]
static PRIOR_BYTES: AtomicUsize = AtomicUsize::new(0);

struct SlabBumpAllocator;

unsafe impl GlobalAlloc for SlabBumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size();
        let align = layout.align().max(1);

        // Two attempts: fit in the current slab; if not, install a new
        // slab sized to fit, and try once more. If that still fails the
        // request is too big for the whole data window — bail.
        for _ in 0..2 {
            let cursor = CURRENT_CURSOR.load(Ordering::SeqCst);
            let end = CURRENT_END.load(Ordering::SeqCst);

            if cursor != 0 {
                // Align up to the request's alignment.
                let aligned = match align_up(cursor, align) {
                    Some(v) => v,
                    None => return null_mut(),
                };
                let new_cursor = match aligned.checked_add(size) {
                    Some(v) => v,
                    None => return null_mut(),
                };
                if new_cursor <= end {
                    CURRENT_CURSOR.store(new_cursor, Ordering::SeqCst);
                    return aligned as *mut u8;
                }
                // Request doesn't fit in the current slab. Fall through
                // and install a fresh one, then loop back to retry.
                //
                // The leftover bytes in the current slab become wasted
                // internal fragmentation. Bump allocators always have
                // this — no compaction in M33.5.
            }

            if !install_new_slab(size) {
                return null_mut();
            }
        }
        null_mut()
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocator: free is a no-op. Slabs are reclaimed by the
        // kernel at task exit via `mem_node_ids` cleanup. See module
        // docs, "Scope cuts".
    }
}

/// Align `v` up to the next multiple of `align`, which must be a
/// power of two (Rust's `Layout` guarantees this). Returns `None` on
/// overflow.
#[inline]
fn align_up(v: usize, align: usize) -> Option<usize> {
    // Power-of-two alignment: `(v + align - 1) & !(align - 1)`, guarded
    // for overflow.
    let mask = align - 1;
    v.checked_add(mask).map(|x| x & !mask)
}

/// Request a fresh slab from the kernel sized to at least `min_size`,
/// and install it as the current slab. Returns `true` on success.
///
/// Called from inside `alloc`; must not allocate itself.
#[inline(never)]
fn install_new_slab(min_size: usize) -> bool {
    let count = SLAB_COUNT.load(Ordering::SeqCst);
    if count >= MAX_SLABS {
        return false;
    }

    // The kernel guarantees 4 KiB-aligned VAs from `map_node`, which
    // satisfies every `Layout::align()` up to a page. Callers asking
    // for larger-than-page alignment are out of scope for M33.5.
    let slab_size = min_size.max(SLAB_DEFAULT);

    // Roll the previous slab (if any) into `prior_bytes` for `used()`
    // reporting. The wasted tail counts as used — the task took those
    // bytes from the kernel whether it bumped into them or not.
    let prior_base = CURRENT_BASE.load(Ordering::SeqCst);
    if prior_base != 0 {
        let prior_end = CURRENT_END.load(Ordering::SeqCst);
        let prior_size = prior_end.saturating_sub(prior_base);
        PRIOR_BYTES.fetch_add(prior_size, Ordering::SeqCst);
    }

    let ptr = match map_node(slab_size) {
        Ok(p) => p.as_ptr() as usize,
        Err(_) => return false,
    };
    // `map_node` rounds size up to 4 KiB internally; mirror that here
    // so `CURRENT_END` matches the kernel's actual mapping.
    let pages = slab_size.div_ceil(4096);
    let total = pages * 4096;

    CURRENT_BASE.store(ptr, Ordering::SeqCst);
    CURRENT_END.store(ptr + total, Ordering::SeqCst);
    CURRENT_CURSOR.store(ptr, Ordering::SeqCst);
    SLAB_COUNT.store(count + 1, Ordering::SeqCst);
    true
}

#[global_allocator]
static GLOBAL: SlabBumpAllocator = SlabBumpAllocator;

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

/// Total dynamic bytes the task has claimed from the kernel so far —
/// the sum of all abandoned slab sizes plus the current slab's used
/// cursor. Useful for demo programs; over-counts by internal alignment
/// padding.
pub fn used() -> usize {
    let prior = PRIOR_BYTES.load(Ordering::SeqCst);
    let cursor = CURRENT_CURSOR.load(Ordering::SeqCst);
    let base = CURRENT_BASE.load(Ordering::SeqCst);
    let current_used = cursor.saturating_sub(base);
    prior.saturating_add(current_used)
}

/// Total capacity of *currently installed* slabs in bytes. Grows as
/// slabs get chained; reaches 0 on a brand-new task that hasn't
/// allocated anything yet.
pub fn capacity() -> usize {
    let prior = PRIOR_BYTES.load(Ordering::SeqCst);
    let base = CURRENT_BASE.load(Ordering::SeqCst);
    let end = CURRENT_END.load(Ordering::SeqCst);
    let current_cap = end.saturating_sub(base);
    prior.saturating_add(current_cap)
}

/// Number of slabs the allocator has requested from the kernel so far.
///
/// Also equals the number of `write` edges the task has to
/// `NodeType::Memory` nodes (the slab-alloc path is the only producer
/// of those edges inside helios-std). User programs can cross-check by
/// calling [`crate::graph::list_edges`] on [`crate::task::self_id`].
pub fn slab_count() -> usize {
    SLAB_COUNT.load(Ordering::SeqCst)
}
