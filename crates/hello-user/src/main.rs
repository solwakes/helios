//! `hello-user` — the first Rust program to run in Helios U-mode.
//!
//! This is the M31 canary. If this runs, Rust user-space is live:
//! syscalls, graph introspection, allocator-backed `String`/`format!`,
//! and `println!` all work on top of `helios-std`.
//!
//! The demo:
//!   1. Prints a greeting.
//!   2. Calls `self_id()` and prints the task's own node id.
//!   3. Enumerates its own outgoing edges via `SYS_LIST_EDGES` and
//!      prints how many it has.
//!   4. Deliberately reads a node we have no `read` cap for and
//!      verifies the typed wrapper returns `Err(Errno::Perm)` rather
//!      than panicking or silently succeeding.
//!   5. Exits cleanly via `SYS_EXIT(0)`.
//!
//! Spawned via `spawn hello` in the kernel shell. The task is granted
//! a `traverse` edge back to itself (so `list_edges(me)` works) plus
//! an `exec` edge to its code node.

#![no_std]
#![no_main]

extern crate alloc;

use helios_std::prelude::*;

helios_std::helios_entry!(main);

fn main() {
    // 1. Greeting.
    helios_std::println!("hello from rust userspace!");

    // 2. Who am I?
    let me = self_id();
    helios_std::println!("my id is {}", me);

    // 3. Count and dump my outgoing edges. `list_edges` returns a
    //    `Vec<EdgeInfo>` — heap-allocated, so this round-trips through
    //    the helios-std bump allocator.
    match list_edges(me) {
        Ok(edges) => {
            let n = edges.len();
            helios_std::println!(
                "my {} outgoing edge{}:",
                n,
                if n == 1 { "" } else { "s" },
            );
            for e in &edges {
                helios_std::println!("  -> {} [{}]", e.target, e.label);
            }
        }
        Err(e) => {
            helios_std::println!("list_edges failed: {}", e);
        }
    }

    // 4. Deliberate cap violation — prove `Errno::Perm` flows back
    //    through the typed `Result` rather than panicking. We have no
    //    `read` edge to the kernel root (#1); this call MUST return
    //    `Err(Errno::Perm)`. The kernel will log the violation to the
    //    console but the task continues.
    let mut scratch = [0u8; 16];
    match read_node(NodeId(1), &mut scratch) {
        Ok(n) => helios_std::println!(
            "[BUG] read_node(#1) returned {} bytes — expected EPERM",
            n,
        ),
        Err(Errno::Perm) => {
            helios_std::println!("read_node(#1) refused with EPERM — caps work.");
        }
        Err(other) => helios_std::println!(
            "read_node(#1) failed with {} (expected EPERM)",
            other,
        ),
    }

    // 5. Heap high-water mark, just to show the allocator did real work
    //    (all those `format!` calls went through it).
    helios_std::println!(
        "(helios-std heap: {} / {} bytes used)",
        helios_std::heap::used(),
        helios_std::heap::capacity(),
    );
}
