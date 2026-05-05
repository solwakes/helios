//! `gtree-user` — recursive graph walker (M34+).
//!
//! Like Unix `tree`, but for the Helios graph: starts at a target node
//! and prints the subtree reachable via `child` edges, up to a fixed
//! depth. Cap edges (`read`/`write`/`exec`/`traverse`) and structural
//! backlinks (`parent`, `self`, etc.) are listed inline as leaf
//! annotations but not recursed into — the tree shape we care about is
//! the parent→child hierarchy, not the full multigraph.
//!
//! # Cap model
//!
//! `gtree` needs a `traverse` cap on every node it recurses into. The
//! shell pre-grants all such caps at spawn time by walking the same
//! subtree kernel-side. If `list_edges` returns `EPERM` on a child
//! (depth budget exhausted, child crosses a cap boundary), the line
//! prints `(no traverse cap)` and the descent stops.
//!
//! # Output
//!
//! ```text
//! gtree #1 (depth=3)
//! #1
//! ├── child     -> #2
//! │   ├── child     -> #5
//! │   └── child     -> #6
//! ├── child     -> #3
//! └── exec      -> #88   (cap, not recursed)
//!
//!   (4 nodes, 7 edges, 0 EPERM)
//! ```
//!
//! # Usage
//!
//! `spawn gtree [id] [depth]` — defaults `id=1` (root), `depth=3`.

#![no_std]
#![no_main]

extern crate alloc;

// Macro/value namespaces don't cross-shadow in Rust — see the same
// note in `ls-user/src/main.rs` for why we re-import the macro.
use helios_std::print;
use helios_std::println;
use helios_std::prelude::*;

helios_std::helios_entry!(main);

/// Total nodes & edges visited; tally of EPERM hits.
#[derive(Default)]
struct Counts {
    nodes: u64,
    edges: u64,
    eperm: u64,
}

fn main() {
    // Args: target node id (a0), max depth (a1).
    //
    // Following ls-user's convention, a0 == 0 means "default to root".
    // a1 == 0 means "default depth", since 0 would mean "show only the
    // root with no edges" — not useful enough to surface as a literal.
    let (a0, a1) = args();
    let target = if a0 == 0 { NodeId(1) } else { NodeId(a0 as u64) };
    let depth = if a1 == 0 { 3u32 } else { a1 as u32 };

    println!("gtree {} (depth={})", target, depth);
    println!("{}", target);

    let mut visited: Vec<NodeId> = Vec::new();
    let mut prefix: Vec<bool> = Vec::new();
    let mut counts = Counts::default();
    visited.push(target);

    walk_edges(target, depth, &mut prefix, &mut visited, &mut counts);

    println!();
    println!(
        "  ({} nodes, {} edges, {} EPERM)",
        counts.nodes, counts.edges, counts.eperm,
    );
}

/// List `id`'s outgoing edges and recurse on `child` edges. Other edges
/// (cap edges, parent backlinks, anything that isn't `child`) print as
/// leaf lines without descent.
fn walk_edges(
    id: NodeId,
    depth_remaining: u32,
    prefix: &mut Vec<bool>,
    visited: &mut Vec<NodeId>,
    counts: &mut Counts,
) {
    counts.nodes = counts.nodes.saturating_add(1);

    if depth_remaining == 0 {
        return;
    }

    let edges = match list_edges(id) {
        Ok(e) => e,
        Err(Errno::Perm) => {
            // Should be rare — the shell pre-granted caps. But if this
            // fires, surface it clearly so the cap-budget mismatch is
            // visible.
            print_prefix(prefix);
            println!("    (no traverse cap to {} — list_edges EPERM)", id);
            counts.eperm = counts.eperm.saturating_add(1);
            return;
        }
        Err(Errno::NotFound) => {
            print_prefix(prefix);
            println!("    ENOENT — no such node {}", id);
            return;
        }
        Err(other) => {
            print_prefix(prefix);
            println!("    {} listing {}", other, id);
            return;
        }
    };

    if edges.is_empty() {
        return;
    }

    counts.edges = counts.edges.saturating_add(edges.len() as u64);

    let total = edges.len();
    for (i, edge) in edges.iter().enumerate() {
        let is_last = i == total - 1;

        // Decode the label: cap-kind labels are reported by the kind
        // byte; structural labels (`child`, `parent`, etc.) come back
        // as Unknown and need a SYS_READ_EDGE_LABEL roundtrip.
        let label_str = match edge.label {
            Label::Unknown(_) => {
                read_edge_label(id, i).unwrap_or_else(|_| String::from("?"))
            }
            other => String::from(other.as_str()),
        };

        // Print the edge line.
        print_prefix(prefix);
        print!("{}", branch(is_last));
        print_label_padded(&label_str);
        println!(" -> {}", edge.target);

        // Recurse only on `child` edges, and only if we haven't visited
        // this target yet (cycle guard — Helios graphs allow back-
        // pointers; we do *not* want to descend through one and loop).
        if label_str == "child" {
            if visited.contains(&edge.target) {
                prefix.push(!is_last);
                print_prefix(prefix);
                println!("    (cycle — already shown)");
                prefix.pop();
            } else {
                visited.push(edge.target);
                prefix.push(!is_last);
                walk_edges(edge.target, depth_remaining - 1, prefix, visited, counts);
                prefix.pop();
            }
        }
    }
}

/// Indent prefix for the current depth. Each entry is "this level had
/// a non-last sibling" — true → `│   `, false → `    `.
fn print_prefix(prefix: &[bool]) {
    for &has_sibling in prefix {
        if has_sibling {
            print!("│   ");
        } else {
            print!("    ");
        }
    }
}

/// Tree-branch glyphs: `├── ` for non-last children, `└── ` for the
/// last child. ASCII alternative would be `+-- ` / `\-- `; using the
/// box-drawing glyphs matches Unix `tree -C` and the kernel UART is
/// UTF-8 clean for our usage.
fn branch(is_last: bool) -> &'static str {
    if is_last {
        "└── "
    } else {
        "├── "
    }
}

/// Pad a label to a fixed column so `->` lines up across the tree.
/// Width 9 matches `ls-user`'s "longest kernel-recognised label is
/// `traverse`" choice plus one space.
fn print_label_padded(label: &str) {
    print!("{}", label);
    let mut written = label.len();
    while written < 9 {
        print!(" ");
        written += 1;
    }
}
