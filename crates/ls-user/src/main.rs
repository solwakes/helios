//! `ls-user` — graph-native directory listing (M32).
//!
//! Prints the outgoing edges of a Helios graph node, with their labels
//! decoded. This is the smallest interesting graph-native Rust tool: one
//! syscall (`SYS_LIST_EDGES`, via `helios_std::graph::list_edges`), one
//! cap (`traverse` to the target node), Rust-only.
//!
//! # Usage
//!
//! The kernel shell dispatches `spawn ls <id>` to a fresh task with:
//!   - `exec` edge to the `ls-user` code node
//!   - `traverse` edge to node `<id>`
//! and passes `<id>` in `a0` at entry (recoverable via
//! [`helios_std::task::args`]). With no argument, the shell passes `1`
//! (the graph root).
//!
//! # What we're showing
//!
//! In Unix, `ls` lists a directory's children — paths in a byte stream.
//! In Helios, "a directory" isn't special: it's any node you have a
//! `traverse` cap to. "Children" aren't special either: they're edges
//! labelled `child`. But the graph also carries `read`, `write`,
//! `exec`, `traverse`, and arbitrary custom labels — `ls-user` dumps
//! *all* of them. So the output is richer than Unix `ls` on purpose:
//! the structure of the graph is the information.

#![no_std]
#![no_main]

extern crate alloc;

// `println!` is a macro and has to be imported separately from the
// prelude glob — Rust's macro / value namespaces don't cross-shadow,
// so the `println` *function* pulled in by `use prelude::*` doesn't
// make the `println!` macro visible. See `helios-std/src/prelude.rs`
// for the note explaining this.
use helios_std::println;
use helios_std::prelude::*;

helios_std::helios_entry!(main);

fn main() {
    // Argument: target node id (via kernel's U-mode a0 passthrough).
    let (a0, _a1) = args();
    let target = if a0 == 0 { NodeId(1) } else { NodeId(a0 as u64) };

    println!("ls {}", target);

    match list_edges(target) {
        Ok(edges) if edges.is_empty() => {
            println!("  (no outgoing edges)");
        }
        Ok(edges) => {
            // Group by label for a tidy output, while preserving kernel
            // order *within* each label group.
            //
            // The output format:
            //     <label> -> #<id>
            // e.g.
            //     child    -> #2
            //     child    -> #3
            //     traverse -> #23
            //
            // We pick a narrow column for the label — longest kernel-
            // recognised label is "traverse" (8 chars). Unknown labels
            // print as "?" so we never widen unpredictably.
            println!("  {} edge{}:", edges.len(), if edges.len() == 1 { "" } else { "s" });
            for (i, e) in edges.iter().enumerate() {
                // M34: SYS_LIST_EDGES only reports cap-kind (read/write/
                // exec/traverse/unknown). Structural labels like `child`,
                // `parent`, `self` all come back as Unknown. For those
                // we issue a second syscall (SYS_READ_EDGE_LABEL) to
                // fetch the full string. On error we fall back to `?`.
                match e.label {
                    Label::Unknown(_) => {
                        let name = read_edge_label(target, i)
                            .unwrap_or_else(|_| String::from("?"));
                        println!("    {:<9} -> {}", name, e.target);
                    }
                    _ => {
                        println!("    {:<9} -> {}", e.label, e.target);
                    }
                }
            }

            // Tally kinds — useful when enumerating dense nodes.
            let mut n_read = 0;
            let mut n_write = 0;
            let mut n_exec = 0;
            let mut n_trav = 0;
            let mut n_other = 0;
            for e in &edges {
                match e.label {
                    Label::Read => n_read += 1,
                    Label::Write => n_write += 1,
                    Label::Exec => n_exec += 1,
                    Label::Traverse => n_trav += 1,
                    Label::Unknown(_) => n_other += 1,
                }
            }
            if n_read + n_write + n_exec + n_trav + n_other > 0 {
                println!(
                    "  (cap: read={} write={} exec={} traverse={} other={})",
                    n_read, n_write, n_exec, n_trav, n_other,
                );
            }
        }
        Err(Errno::Perm) => {
            println!("  ls: EPERM (no traverse cap to {})", target);
            exit(1);
        }
        Err(Errno::NotFound) => {
            println!("  ls: no such node {}", target);
            exit(2);
        }
        Err(other) => {
            println!("  ls: {}", other);
            exit(3);
        }
    }
}
