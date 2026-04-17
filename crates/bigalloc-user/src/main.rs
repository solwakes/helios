//! `bigalloc-user` — proves helios-std's `GlobalAlloc` is backed by
//! `SYS_MAP_NODE` slabs (M33.5).
//!
//! Before M33.5, every Rust user task carried a 64 KiB `[0xAA; N]`
//! bump arena inside its `.data` image. M33 shipped `SYS_MAP_NODE`
//! but left `GlobalAlloc` pointing at the in-binary arena. M33.5
//! rewires `GlobalAlloc` so `alloc::Vec::push` / `alloc::String`
//! implicitly request fresh slabs from the kernel as they grow.
//!
//! This demo exercises the new path:
//!
//! 1. Allocate a `Vec<u64>` sized `>= 16 KiB` via `with_capacity`,
//!    fill it with a word pattern, verify every word.
//! 2. Allocate a second `Vec<u64>` strictly larger than
//!    [`helios_std::heap::SLAB_DEFAULT`] (16 KiB) — forcing the
//!    allocator to request a second slab sized to fit — and verify
//!    that too.
//! 3. Print `list_edges(self_id())` and count the outgoing `write`
//!    edges. Each `map_node` slab shows up as one such edge from the
//!    task node to a `NodeType::Memory` node, so the count must be
//!    at least 2 — direct proof that the allocator is backed by real
//!    kernel-managed memory rather than in-binary bytes.
//!
//! Note on slab vs data-window sizing. The task's total data VA window
//! is 64 KiB (see `USER_DATA_MAX_PAGES` in `src/user.rs`). The second
//! allocation is 32 KiB, not the "96 KiB" the M33.5 spec used as a
//! size hint — 96 KiB is above the window ceiling and would return
//! `ENOMEM`. 32 KiB is the largest round-number size that still fits
//! alongside a 16 KiB first allocation (16 + 32 = 48 ≤ 64) and is
//! strictly larger than `SLAB_DEFAULT`, which is the property the
//! test actually cares about.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;

use helios_std::heap;
use helios_std::prelude::*;

helios_std::helios_entry!(main);

/// Word pattern for allocation A. Distinctive `u64` so a stray byte
/// flip anywhere is obvious in a debugger dump.
const PATTERN_A: u64 = 0x0123_4567_89AB_CDEF;

/// Word pattern for allocation B.
const PATTERN_B: u64 = 0xDEAD_BEEF_FEED_FACE;

fn main() {
    println!("bigalloc: helios-std GlobalAlloc via SYS_MAP_NODE slabs (M33.5)");
    println!(
        "  slab-default: {} bytes; max-slabs: {}",
        heap::SLAB_DEFAULT,
        heap::MAX_SLABS,
    );
    println!(
        "  pre-alloc heap: used={}, capacity={}, slabs={}",
        heap::used(),
        heap::capacity(),
        heap::slab_count(),
    );

    // -- Allocation A: >= 16 KiB Vec<u64> with a word pattern. --------
    //
    // `with_capacity(2048)` asks Rust for exactly 16384 bytes
    // (8 bytes/u64 × 2048). Our allocator rounds up to the slab
    // granularity, which means a fresh 16 KiB slab exactly fills.
    let a_words = 2048usize;
    let a_bytes = a_words * core::mem::size_of::<u64>();
    assert!(a_bytes >= 16 * 1024);

    let mut a: Vec<u64> = Vec::with_capacity(a_words);
    for i in 0..a_words {
        a.push(PATTERN_A ^ (i as u64));
    }
    let mut mismatches = 0usize;
    for i in 0..a_words {
        let want = PATTERN_A ^ (i as u64);
        if a[i] != want {
            mismatches += 1;
            if mismatches <= 3 {
                println!(
                    "    A mismatch at word {}: got {:#018x}, want {:#018x}",
                    i, a[i], want,
                );
            }
        }
    }
    if mismatches != 0 {
        println!("  A: FAIL — {} word mismatch(es)", mismatches);
        exit(2);
    }
    println!(
        "  A: Vec<u64> cap={} ({} bytes) filled+verified; ptr={:#x}",
        a_words,
        a_bytes,
        a.as_ptr() as usize,
    );
    println!(
        "    after A: used={}, capacity={}, slabs={}",
        heap::used(),
        heap::capacity(),
        heap::slab_count(),
    );

    // -- Allocation B: strictly larger than one slab. -----------------
    //
    // 32 KiB forces the allocator to skip the remainder of slab 1 and
    // request a second, larger slab sized to fit. This is the slab
    // chaining path.
    let b_words = 4096usize;
    let b_bytes = b_words * core::mem::size_of::<u64>();
    assert!(b_bytes > heap::SLAB_DEFAULT);

    let mut b: Vec<u64> = Vec::with_capacity(b_words);
    for i in 0..b_words {
        b.push(PATTERN_B ^ (i as u64));
    }
    let mut b_mismatches = 0usize;
    for i in 0..b_words {
        let want = PATTERN_B ^ (i as u64);
        if b[i] != want {
            b_mismatches += 1;
            if b_mismatches <= 3 {
                println!(
                    "    B mismatch at word {}: got {:#018x}, want {:#018x}",
                    i, b[i], want,
                );
            }
        }
    }
    if b_mismatches != 0 {
        println!("  B: FAIL — {} word mismatch(es)", b_mismatches);
        exit(3);
    }
    println!(
        "  B: Vec<u64> cap={} ({} bytes) filled+verified; ptr={:#x}",
        b_words,
        b_bytes,
        b.as_ptr() as usize,
    );
    println!(
        "    after B: used={}, capacity={}, slabs={}",
        heap::used(),
        heap::capacity(),
        heap::slab_count(),
    );

    // -- Cross-check: A was not clobbered by B's allocation. ----------
    let mut cross = 0usize;
    for i in 0..a_words {
        let want = PATTERN_A ^ (i as u64);
        if a[i] != want {
            cross += 1;
        }
    }
    if cross != 0 {
        println!("  FAIL: A was disturbed by B's allocation ({} mismatch(es))", cross);
        exit(4);
    }
    println!("  A: still intact after B-alloc — slabs are disjoint.");

    // -- Slab-count invariant + graph inspection. ---------------------
    //
    // If slab chaining actually happened, we must have requested at
    // least 2 slabs from the kernel. Each one minted a `Memory` node
    // and a `write` self→memory edge.
    let slabs = heap::slab_count();
    println!("  heap reports {} slab(s) installed", slabs);
    if slabs < 2 {
        println!("  FAIL: expected >= 2 slabs after chaining, got {}", slabs);
        exit(5);
    }

    let me = self_id();
    println!("  self id: {}", me);
    let edges = match list_edges(me) {
        Ok(e) => e,
        Err(e) => {
            // The task spawns with a self-traverse cap, so list_edges
            // on self should always succeed; if it didn't, something
            // is wrong with the shell-side wiring, not the allocator.
            println!("  list_edges(self) failed: {}", e);
            exit(6);
        }
    };
    let mut writes = 0usize;
    println!("  outgoing edges ({}):", edges.len());
    for (i, e) in edges.iter().enumerate() {
        let label = match e.label {
            Label::Unknown(_) => {
                // Structural edges (child/parent/self) show up as
                // Unknown via list_edges; resolve the string form via
                // SYS_READ_EDGE_LABEL so the dump is readable.
                match helios_std::graph::read_edge_label(me, i) {
                    Ok(s) => s,
                    Err(_) => alloc::string::String::from("?"),
                }
            }
            other => alloc::string::String::from(other.as_str()),
        };
        println!("    [{}] -> {} [{}]", i, e.target, label);
        if matches!(e.label, Label::Write) {
            writes += 1;
        }
    }

    if writes < slabs {
        println!(
            "  FAIL: heap claims {} slab(s) but only {} write-edge(s) visible",
            slabs, writes,
        );
        exit(7);
    }
    println!(
        "  OK: {} write-edge(s) to Memory nodes match {} installed slab(s).",
        writes, slabs,
    );

    println!(
        "bigalloc: OK — alloc::Vec is backed by SYS_MAP_NODE, chaining works."
    );
}
