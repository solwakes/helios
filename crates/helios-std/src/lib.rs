//! `helios-std` — the Rust-native "libc" for Helios user-mode programs.
//!
//! This crate is what you link against instead of `libc` when targeting
//! Helios. It exposes the kernel's graph-capability ABI as typed Rust:
//!
//! - [`sys`] — raw `ecall` wrappers for the 9 Helios syscalls
//!   (`SYS_READ_NODE`, `SYS_WRITE_NODE`, `SYS_LIST_EDGES`,
//!   `SYS_FOLLOW_EDGE`, `SYS_SELF`, `SYS_PRINT`, `SYS_EXIT`,
//!   `SYS_MAP_NODE`, `SYS_READ_EDGE_LABEL`).
//! - [`graph`] — typed primitives: [`NodeId`][graph::NodeId],
//!   [`Label`][graph::Label] (alias: [`LabelKind`][graph::LabelKind]),
//!   [`Edge`][graph::Edge], plus wrappers that return
//!   `Result<_, Errno>` instead of raw return codes.
//! - [`io`] — `print`/`println` plus a [`io::Stdout`] implementing
//!   [`core::fmt::Write`], so `write!`/`writeln!` Just Work.
//! - [`task`] — [`task::self_id`], [`task::exit`], [`task::args`].
//! - [`heap`] — the global bump allocator backing `alloc::*`.
//! - [`prelude`] — one-stop import: `use helios_std::prelude::*;`.
//!
//! # Thesis alignment
//!
//! No POSIX. No file descriptors. No paths. Everything is graph ops
//! gated by capability edges. See
//! [`docs/userspace/rust-std.md`](https://github.com/solwakes/helios/blob/main/docs/userspace/rust-std.md)
//! for the design rationale, and
//! [`docs/design/capability-edges.md`](https://github.com/solwakes/helios/blob/main/docs/design/capability-edges.md)
//! for the syscall ABI contract this crate wraps.
//!
//! # Example
//!
//! ```ignore
//! #![no_std]
//! #![no_main]
//! extern crate alloc;
//!
//! helios_std::helios_entry!(main);
//!
//! use helios_std::prelude::*;
//!
//! fn main() {
//!     println!("hello from rust userspace!");
//!     let me = self_id();
//!     println!("my id is {}", me);
//! }
//! ```
//!
//! # M31/M33 caveats (stopgaps until follow-on milestones)
//!
//! - **64 KiB bump heap, no free.** The heap still lives inside the
//!   binary image for now — M33 adds [`graph::map_node`] for explicit
//!   slabs of dynamic memory, but the `GlobalAlloc` backing for
//!   `alloc::*` has not yet been rerouted through it. That integration
//!   is a follow-on milestone; until then, `Vec`/`String`/`format!`
//!   share the binary-embedded arena (trackable via [`heap::used`] /
//!   [`heap::capacity`]), and user programs that want bigger working
//!   sets should call [`graph::map_node`] directly.
//! - **W^X at the task level is waived.** Exec edges are mapped R+W+X+U
//!   so initialized data can be patched at load time. Cross-task caps
//!   are still strictly MMU-enforced (no edge → no mapping → no access).
//! - **No argv/env.** Tasks receive two `usize` args (a0, a1) from the
//!   spawner; see [`task::args`]. A proper "spawn context as a graph
//!   subnode" scheme belongs to a later milestone.

#![no_std]
#![allow(clippy::missing_safety_doc)]

extern crate alloc;

pub mod sys;
pub mod graph;
pub mod io;
pub mod task;
pub mod heap;
pub mod prelude;

// Re-export commonly-used names at the crate root for ergonomics.
pub use graph::{Edge, EdgeInfo, Errno, Label, LabelKind, NodeId};
pub use task::{self_id, exit};

// The `helios_entry!` macro below is what user binaries use to wire up
// `_start` + a panic handler. It expands to items at the user crate's
// root so the symbols land correctly (libraries can't define a
// `#[panic_handler]` themselves).

/// Wraps a `fn main()` in the `_start` glue the Helios kernel expects.
///
/// The kernel jumps to the first byte of a task's first `exec` edge
/// (VA `0x40000000`). The hello-user linker script places
/// `.text.entry` first, so `_start` lives there. `_start`:
///
/// 1. Stashes the kernel-passed `a0`/`a1` so [`task::args`] can recover
///    them later.
/// 2. Calls the user's `main`.
/// 3. Issues `SYS_EXIT(0)` if `main` returns normally.
///
/// Also installs a `#[panic_handler]` that prints the panic message via
/// `SYS_PRINT` and then calls `SYS_EXIT(1)`.
///
/// # Usage
///
/// ```ignore
/// #![no_std]
/// #![no_main]
/// extern crate alloc;
///
/// helios_std::helios_entry!(main);
///
/// fn main() { /* ... */ }
/// ```
#[macro_export]
macro_rules! helios_entry {
    ($main:ident) => {
        #[no_mangle]
        #[link_section = ".text.entry"]
        pub extern "C" fn _start(a0: usize, a1: usize) -> ! {
            // Stash kernel-passed args so task::args() can retrieve them.
            // Safe here: we're on the only hart, pre-main.
            $crate::task::__set_entry_args(a0, a1);
            $main();
            $crate::task::exit(0)
        }

        #[panic_handler]
        fn __helios_panic_handler(info: &core::panic::PanicInfo) -> ! {
            use core::fmt::Write as _;
            let mut out = $crate::io::Stdout;
            let _ = out.write_str("panic: ");
            let _ = core::write!(&mut out, "{}", info);
            let _ = out.write_str("\n");
            $crate::task::exit(1)
        }
    };
}
