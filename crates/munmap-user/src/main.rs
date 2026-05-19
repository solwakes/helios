//! `munmap-user` — release a `SYS_MAP_NODE` allocation via the new
//! `SYS_UNMAP_NODE` syscall (post-M35 Proposal B).
//!
//! Proves the new syscall end-to-end:
//!
//! 1. Allocate region A (32 KiB = 8 pages) and fill it with a `u32`
//!    pattern. Allocate region B (8 KiB = 2 pages) and fill with a
//!    distinctive byte. Verify both.
//! 2. Enumerate self's outgoing edges via the *non-allocating*
//!    [`list_edges_into`] form. (Using the [`list_edges`] Vec form
//!    would itself trigger a `map_node` call via helios-std's
//!    GlobalAlloc, adding a third `write` edge for the heap slab —
//!    the demo's invariants must be robust to that, but for the
//!    captures we want a clean view, so the stack-buffered form is
//!    used.) Capture A's NodeId (first `write`) and B's NodeId
//!    (second `write`).
//! 3. Call `unmap_node(A)`. Re-enumerate edges; A's `write` edge
//!    must be gone (membership test) and B's must remain.
//! 4. Touch B — re-fill and verify. The B mapping must survive the
//!    A-unmap (proves the kernel zaps only A's PT slots).
//! 5. Call `unmap_node(A)` again — must return `Errno::NotFound`. A
//!    is no longer in the task's allocated set.
//! 6. Allocate region C with the same size as A. C's base VA must
//!    equal A's old base — proving the data-window slots A occupied
//!    were truly returned to the kernel's slot bitmap and not leaked.
//!
//! Note that `list_edges_into` reads up to 32 edges per call into a
//! stack-allocated 16-byte-per-edge staging buffer — 512 bytes — so
//! no heap is needed for these probes. Helios-std's heap allocator
//! may *still* mint slabs implicitly (e.g. if we formatted any
//! string through `alloc::format!`), but this demo doesn't.
//!
//! See `docs/design/capability-edges.md` (post-M35 Implementation
//! Notes) for the kernel-side semantics this exercises.

#![no_std]
#![no_main]

extern crate alloc;

use helios_std::graph::list_edges_into;
use helios_std::prelude::*;

helios_std::helios_entry!(main);

/// Pattern for region A — a distinctive `u32` so a stray write
/// anywhere is obvious in a debugger dump.
const PATTERN_A: u32 = 0xCAFE_F00D;
/// Byte fill for region B.
const PATTERN_B: u8 = 0xA5;
/// Pattern for region C (post-reuse). Different from A so we can
/// verify the data is fresh, not lingering bytes.
const PATTERN_C: u32 = 0x1337_BEEF;

/// Staging buffer for `list_edges_into`. Sized to the max the
/// underlying syscall returns per call (LIST_EDGES_STAGE = 32 in
/// helios-std). The task in question has only a handful of outgoing
/// edges (exec edges + a couple of write edges + the self-traverse
/// edge), so 32 slots is more than enough.
const EDGE_PROBE_MAX: usize = 32;

fn main() {
    println!("munmap: exercising SYS_UNMAP_NODE (post-M35 Proposal B)");

    let me = self_id();
    println!("  self id: {}", me);

    // -- Step 1: allocate A (32 KiB) and B (8 KiB). -------------------
    let a_bytes = 32 * 1024usize;
    let a = match map_node_slice(a_bytes) {
        Ok(s) => s,
        Err(e) => fail("map_node(A, 32 KiB)", e, 1),
    };
    let a_base = a.as_ptr() as usize;
    let a_end = a_base + a.len();
    fill_u32(a, PATTERN_A);
    if let Err(idx) = check_u32(a, PATTERN_A) {
        println!("  A: post-fill mismatch at word {}", idx);
        exit(2);
    }
    println!(
        "  A: filled+verified {} bytes at {:#010x}..{:#010x}",
        a.len(),
        a_base,
        a_end,
    );

    let b_bytes = 8 * 1024usize;
    let b = match map_node_slice(b_bytes) {
        Ok(s) => s,
        Err(e) => fail("map_node(B, 8 KiB)", e, 3),
    };
    let b_base = b.as_ptr() as usize;
    let b_end = b_base + b.len();
    for byte in b.iter_mut() {
        *byte = PATTERN_B;
    }
    for (i, &byte) in b.iter().enumerate() {
        if byte != PATTERN_B {
            println!("  B: post-fill mismatch at byte {}", i);
            exit(4);
        }
    }
    println!(
        "  B: filled+verified {} bytes at {:#010x}..{:#010x}",
        b.len(),
        b_base,
        b_end,
    );

    // -- Step 2: enumerate self's outgoing edges, find A and B. -------
    //
    // Use the *non-allocating* form so the probe doesn't introduce a
    // new write edge of its own. With no pre-granted read / write
    // caps at spawn, the first two `write` edges we see *are* A and B.
    let (a_id, b_id) = match find_first_two_writes(me) {
        Some(pair) => pair,
        None => {
            println!("  FAIL: expected ≥2 write-edges on self, didn't find them");
            exit(5);
        }
    };
    println!("  edges: A = {}, B = {}", a_id, b_id);

    // -- Step 3: unmap_node(A). ---------------------------------------
    if let Err(e) = unmap_node(a_id) {
        println!("  unmap_node(A={}) failed: {}", a_id, e);
        exit(6);
    }
    println!("  unmap_node(A={}) OK", a_id);

    // -- Step 4a: after unmap, A's edge is GONE and B's edge REMAINS.
    //              The total count may not be exactly 1 (other write
    //              edges may exist — e.g. the helios-std heap allocator
    //              minted slabs of its own during this run). We assert
    //              the two surgical facts and ignore the rest.
    if write_edge_present(me, a_id) {
        println!(
            "  FAIL: A's write-edge (target #{}) still present after unmap(A)",
            a_id,
        );
        exit(7);
    }
    if !write_edge_present(me, b_id) {
        println!(
            "  FAIL: B's write-edge (target #{}) gone after unmap(A)",
            b_id,
        );
        exit(8);
    }
    println!("  edge invariants after unmap(A): A gone, B present. OK.");

    // -- Step 4b: B still works — re-fill and verify. -----------------
    //
    // This is the load-bearing test. If the kernel zapped too much
    // (e.g. flushed B's PT alongside A's), the next byte we touch in B
    // would fault. Cross-check that the surgical PT zap only hit A.
    for byte in b.iter_mut() {
        // Bit-flip every byte, then flip back — exercises both writes
        // and reads through B's PT mapping.
        *byte ^= 0xFF;
    }
    let want = PATTERN_B ^ 0xFF;
    for (i, &byte) in b.iter().enumerate() {
        if byte != want {
            println!("  FAIL: B disturbed at byte {} after unmap(A)", i);
            exit(9);
        }
    }
    for byte in b.iter_mut() {
        *byte ^= 0xFF;
    }
    for (i, &byte) in b.iter().enumerate() {
        if byte != PATTERN_B {
            println!("  FAIL: B restoration mismatch at byte {}", i);
            exit(10);
        }
    }
    println!("  B: write+verify after unmap(A) still works. PT survived.");

    // -- Step 5: repeat unmap_node(A) — must be ENOENT. ---------------
    match unmap_node(a_id) {
        Ok(()) => {
            println!("  FAIL: second unmap_node(A) succeeded; expected ENOENT");
            exit(11);
        }
        Err(Errno::NotFound) => {
            println!("  unmap_node(A) twice: second call -> NotFound. OK.");
        }
        Err(e) => {
            println!("  FAIL: second unmap_node(A) -> {}; expected NotFound", e);
            exit(12);
        }
    }

    // -- Step 6: allocate C with A's old size — must reuse A's slots. -
    let c = match map_node_slice(a_bytes) {
        Ok(s) => s,
        Err(e) => fail("map_node(C, 32 KiB)", e, 13),
    };
    let c_base = c.as_ptr() as usize;
    if c_base != a_base {
        println!(
            "  FAIL: C base {:#010x} != A's old base {:#010x} — slots not reclaimed",
            c_base, a_base,
        );
        exit(14);
    }
    fill_u32(c, PATTERN_C);
    if let Err(idx) = check_u32(c, PATTERN_C) {
        println!("  C: post-fill mismatch at word {}", idx);
        exit(15);
    }
    println!(
        "  C: filled+verified {} bytes at {:#010x} (= A's old base). SLOTS RECLAIMED.",
        c.len(),
        c_base,
    );

    // -- Final invariant: A still gone, B still present, C present. -
    if write_edge_present(me, a_id) {
        println!("  FAIL: A's edge resurrected at end");
        exit(16);
    }
    if !write_edge_present(me, b_id) {
        println!("  FAIL: B's edge missing at end");
        exit(17);
    }
    println!(
        "  final edges: A gone, B present, C present (own check via the live VA). OK."
    );

    println!("munmap: OK — SYS_UNMAP_NODE frees the allocation cleanly + slot is reusable.");
}

/// Snapshot self's outgoing edges into a stack buffer (no allocator
/// traffic). Returns the populated prefix.
fn snapshot_edges(src: NodeId) -> ([EdgeInfo; EDGE_PROBE_MAX], usize) {
    let mut buf: [EdgeInfo; EDGE_PROBE_MAX] = [EdgeInfo {
        target: NodeId(0),
        label: Label::Unknown(0),
    }; EDGE_PROBE_MAX];
    let n = list_edges_into(src, &mut buf).unwrap_or(0);
    (buf, n)
}

/// Find the first two `write` edge targets on `src`. Returns
/// `Some((first, second))` only if there are at least two; otherwise
/// `None`. Uses the stack-buffered list call to avoid minting a heap
/// slab in the middle of the probe.
fn find_first_two_writes(src: NodeId) -> Option<(NodeId, NodeId)> {
    let (buf, n) = snapshot_edges(src);
    let mut iter = buf[..n]
        .iter()
        .filter(|e| matches!(e.label, Label::Write))
        .map(|e| e.target);
    let a = iter.next()?;
    let b = iter.next()?;
    Some((a, b))
}

/// Return true iff a *live* `write` edge from `src` to `target`
/// exists. (Tombstoned edges are filtered out by the kernel's
/// `iter_live` view, so a `Write`-labelled target hit in
/// `list_edges_into` is by definition live.)
fn write_edge_present(src: NodeId, target: NodeId) -> bool {
    let (buf, n) = snapshot_edges(src);
    buf[..n]
        .iter()
        .any(|e| matches!(e.label, Label::Write) && e.target == target)
}

/// Fill `slice` with little-endian copies of `pattern`. Asserts the
/// slice length is a multiple of 4.
fn fill_u32(slice: &mut [u8], pattern: u32) {
    let bytes = pattern.to_le_bytes();
    let mut i = 0;
    while i + 4 <= slice.len() {
        slice[i] = bytes[0];
        slice[i + 1] = bytes[1];
        slice[i + 2] = bytes[2];
        slice[i + 3] = bytes[3];
        i += 4;
    }
}

/// Verify every 4-byte word in `slice` equals `pattern`. Returns
/// `Err(word_index)` for the first mismatch.
fn check_u32(slice: &[u8], pattern: u32) -> Result<(), usize> {
    let words = slice.len() / 4;
    for i in 0..words {
        let o = i * 4;
        let w = u32::from_le_bytes([slice[o], slice[o + 1], slice[o + 2], slice[o + 3]]);
        if w != pattern {
            return Err(i);
        }
    }
    Ok(())
}

fn fail(what: &str, e: Errno, code: i32) -> ! {
    println!("  {}: {}", what, e);
    exit(code);
}
