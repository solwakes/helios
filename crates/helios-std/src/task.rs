//! Task-level primitives: self-introspection, exit, entry-time args.
//!
//! A Helios task is a node in the graph. You identify your own node
//! with [`self_id`] (via `SYS_SELF`), and you end the task with
//! [`exit`] (via `SYS_EXIT`).
//!
//! Spawn-time arguments (`a0`, `a1` as passed by the kernel — see
//! `run_user_task_with_caps` in the kernel) are stashed by the
//! [`helios_entry!`][crate::helios_entry] macro and recoverable via
//! [`args`]. This is the M31 stand-in for proper `argv`/`env`; a
//! graph-native "spawn context" scheme is deferred.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::graph::NodeId;
use crate::sys;

// ---------------------------------------------------------------------------
// Entry-time args: set by helios_entry!()'s _start, read by args().
// ---------------------------------------------------------------------------
//
// Atomic so we don't need unsafe at the read site, even though in
// practice this is single-writer-before-main / many-readers-after.

// The `link_section` attribute forces these into a `.data.*` sub-section
// so the linker script's `*(.data .data.*)` glob picks them up, which
// keeps them inside the raw binary image the kernel copies. A plain
// `static` with a zero initializer can land in `.bss` (NOBITS), which
// `objcopy -O binary` drops from the image — the resulting pages are
// then unmapped and writes fault. See the kernel's `build_user_address_space`
// and the `.bss (NOLOAD)` note in `hello-user/linker.ld`.
#[link_section = ".data.helios_entry_args"]
static ENTRY_A0: AtomicUsize = AtomicUsize::new(0);
#[link_section = ".data.helios_entry_args"]
static ENTRY_A1: AtomicUsize = AtomicUsize::new(0);

/// Stash the `a0`/`a1` values the kernel placed in registers at task
/// entry. Invoked exactly once, by the `_start` shim expanded by
/// [`helios_entry!`][crate::helios_entry].
///
/// Implementation detail; user code should call [`args`] instead.
#[doc(hidden)]
pub fn __set_entry_args(a0: usize, a1: usize) {
    ENTRY_A0.store(a0, Ordering::Relaxed);
    ENTRY_A1.store(a1, Ordering::Relaxed);
}

/// Return the `(a0, a1)` values the kernel handed this task at entry.
///
/// In M31 these are the two `usize` arguments to
/// `run_user_task_with_caps`: typically a node id the task should
/// operate on, or zero.
///
/// Before a proper "spawn context" syscall exists, this is how a task
/// receives its inputs. Beyond the two `usize` slots, everything else
/// comes from the graph: the task's outgoing edges declare what it
/// can see, and `SYS_LIST_EDGES` / `SYS_FOLLOW_EDGE` let it introspect.
pub fn args() -> (usize, usize) {
    (
        ENTRY_A0.load(Ordering::Relaxed),
        ENTRY_A1.load(Ordering::Relaxed),
    )
}

// ---------------------------------------------------------------------------
// Self / exit
// ---------------------------------------------------------------------------

/// Return the caller's own task node id (`SYS_SELF`).
pub fn self_id() -> NodeId {
    let r = unsafe { sys::syscall0(sys::SYS_SELF) };
    NodeId(r as u64)
}

/// Terminate the task with the given exit code (`SYS_EXIT`). Does not
/// return.
pub fn exit(code: i32) -> ! {
    unsafe { sys::syscall_exit(code) }
}
