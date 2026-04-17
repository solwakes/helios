//! `mmap-user` — kernel-granted dynamic memory via `SYS_MAP_NODE` (M33).
//!
//! Proves the new syscall works end-to-end:
//!
//! 1. Ask for 32 KiB — fill it with a distinctive `u32` pattern, read
//!    it back, check every word.
//! 2. Ask for another 8 KiB — memset to `0x55`, read back, check every
//!    byte.
//! 3. Confirm the two VAs don't overlap (they're adjacent slots in
//!    the task's data window, but the point is demonstrating that two
//!    live allocations coexist).
//!
//! The shell launches this as `spawn mmap`. The kernel doesn't give
//! the task any `read`/`write` edges up front — every byte the demo
//! touches lives in a node the *task itself* minted via `map_node`,
//! proving that user programs can grow their own memory under the
//! graph-capability model.
//!
//! See `docs/design/capability-edges.md` ("M33 Implementation Notes")
//! for the kernel-side semantics this exercises.

#![no_std]
#![no_main]

extern crate alloc;

use helios_std::graph::{map_node_slice, Errno};
use helios_std::println;
use helios_std::prelude::*;

helios_std::helios_entry!(main);

/// First-allocation pattern: byte-swap-distinctive `u32`.
const PATTERN_A: u32 = 0xABCDEF01;
/// Second-allocation byte fill.
const PATTERN_B: u8 = 0x55;

fn main() {
    println!("mmap: exercising SYS_MAP_NODE");

    // -- Allocation A: 32 KiB filled with a u32 pattern. --------------
    let a_size = 32 * 1024;
    let a = match map_node_slice(a_size) {
        Ok(s) => s,
        Err(e) => fail("map_node(32 KiB)", e, 1),
    };
    let a_base = a.as_ptr() as usize;
    let a_end = a_base + a.len();
    println!(
        "  A: {:#010x}..{:#010x} ({} bytes, {} page(s))",
        a_base,
        a_end,
        a.len(),
        a.len() / 4096,
    );

    // Fill with the u32 pattern.
    let words = a.len() / 4;
    for i in 0..words {
        let bytes = PATTERN_A.to_le_bytes();
        let o = i * 4;
        a[o] = bytes[0];
        a[o + 1] = bytes[1];
        a[o + 2] = bytes[2];
        a[o + 3] = bytes[3];
    }
    // Verify every word.
    let mut mismatches = 0usize;
    for i in 0..words {
        let o = i * 4;
        let w = u32::from_le_bytes([a[o], a[o + 1], a[o + 2], a[o + 3]]);
        if w != PATTERN_A {
            mismatches += 1;
            if mismatches <= 3 {
                println!(
                    "    mismatch at word {}: got {:#010x}, want {:#010x}",
                    i, w, PATTERN_A,
                );
            }
        }
    }
    if mismatches != 0 {
        println!("  A: FAIL — {} word mismatch(es)", mismatches);
        exit(2);
    }
    println!(
        "  A: filled + verified {} words of {:#010x}",
        words, PATTERN_A,
    );

    // -- Allocation B: 8 KiB memset to 0x55. --------------------------
    let b_size = 8 * 1024;
    let b = match map_node_slice(b_size) {
        Ok(s) => s,
        Err(e) => fail("map_node(8 KiB)", e, 3),
    };
    let b_base = b.as_ptr() as usize;
    let b_end = b_base + b.len();
    println!(
        "  B: {:#010x}..{:#010x} ({} bytes, {} page(s))",
        b_base,
        b_end,
        b.len(),
        b.len() / 4096,
    );

    // Overlap check (belt-and-suspenders — kernel doesn't double-map).
    if a_base < b_end && b_base < a_end {
        println!(
            "  FAIL: A and B overlap: A={:#x}..{:#x}, B={:#x}..{:#x}",
            a_base, a_end, b_base, b_end,
        );
        exit(4);
    }

    // Memset + verify.
    for byte in b.iter_mut() {
        *byte = PATTERN_B;
    }
    let mut b_mismatches = 0usize;
    for (i, &byte) in b.iter().enumerate() {
        if byte != PATTERN_B {
            b_mismatches += 1;
            if b_mismatches <= 3 {
                println!(
                    "    mismatch at byte {}: got {:#04x}, want {:#04x}",
                    i, byte, PATTERN_B,
                );
            }
        }
    }
    if b_mismatches != 0 {
        println!("  B: FAIL — {} byte mismatch(es)", b_mismatches);
        exit(5);
    }
    println!(
        "  B: memset + verified {} bytes of {:#04x}",
        b.len(),
        PATTERN_B,
    );

    // -- Cross-allocation check: writing to B didn't clobber A. -------
    let mut cross_mismatches = 0usize;
    for i in 0..words {
        let o = i * 4;
        let w = u32::from_le_bytes([a[o], a[o + 1], a[o + 2], a[o + 3]]);
        if w != PATTERN_A {
            cross_mismatches += 1;
        }
    }
    if cross_mismatches != 0 {
        println!(
            "  FAIL: A was disturbed by writes to B ({} mismatch(es))",
            cross_mismatches,
        );
        exit(6);
    }
    println!("  A: still intact after B-writes — allocations are disjoint.");

    println!(
        "mmap: OK — {} total bytes mapped via SYS_MAP_NODE across 2 allocation(s).",
        a.len() + b.len(),
    );
}

fn fail(what: &str, e: Errno, code: i32) -> ! {
    println!("  {}: {}", what, e);
    exit(code);
}
