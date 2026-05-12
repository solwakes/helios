//! `cdtsmoke-alpha-user` — CDT (M35) litmus α.
//!
//! Exercises **`SYS_DELEGATE_EDGE` + `SYS_REVOKE_EDGE` within a single
//! task's lifetime**. The companion piece is `cdtsmoke-beta-user`,
//! which exercises **task-exit cascade**. Together the two binaries
//! make each half of the CDT invariant visible:
//!
//!   α: revoke → cascade kills the derived edge ✓
//!   β: exit  → cascade kills the derived edge ✓
//!
//! Each binary's output is structured for `grep -E 'PASS|FAIL'`
//! consumption — the kernel test driver (eventually) parses the
//! stdout. For now `cmd_spawn` prints user output to the console and
//! a human reads the result.
//!
//! # Choreography (α — this binary)
//!
//! Single-hart cooperative scheduler. A runs end-to-end; B never
//! executes. We use B's task node as the *recipient of delegated
//! caps* — observable by listing B's outgoing edges via
//! `SYS_LIST_EDGES` (A holds `traverse` on B).
//!
//! 1. List B's edges. Note `baseline` count of live outgoing edges.
//! 2. Call `delegate_edge(T, B, "write")`. Expect `Ok(_)`.
//! 3. List B's edges again. Expect `baseline + 1`; new edge has
//!    `target == T` and `label == Label::Write`.
//! 4. Call `revoke_edge(T, "write")`. Expect `Ok(2)` — the own edge
//!    + the derived edge on B.
//! 5. List B's edges a third time. Expect `baseline` again (derived
//!    edge tombstoned, list_edges skips tombstones).
//! 6. Exit cleanly with code 0.
//!
//! # Cap model
//!
//! The shell pre-grants the following at spawn:
//!   - `exec` on the binary code node (root)
//!   - `traverse` self (root, optional)
//!   - `traverse` on B (root) — to list B's edges
//!   - `write` on T (root) — the cap we're delegating
//!   - `grant` on T (root) — authorisation to delegate
//!
//! Args: `a0 = T_id`, `a1 = B_task_node_id`.
//!
//! # Output shape
//!
//! ```text
//! cdtsmoke-α: T=#101 B=#102
//!   baseline: B has N live edges
//!   delegate write→B: ok (idx=42)
//!   after delegate: B has N+1 live edges (+1 write→#101 ✓)
//!   revoke write@T: ok (2 edge(s) tombstoned)
//!   after revoke: B has N live edges (derived edge gone ✓)
//! cdtsmoke-α: PASS
//! ```
//!
//! Any unexpected outcome prints `FAIL <reason>` and exits non-zero.
//! No panic, no MMU fault — every failure path is recoverable at the
//! typed-syscall layer.

#![no_std]
#![no_main]

extern crate alloc;

use helios_std::prelude::*;

helios_std::helios_entry!(main);

/// Count B's *live* outgoing edges that are visible via SYS_LIST_EDGES.
/// Also returns whether any one of them matches `(label, target)` —
/// the invariant we want to assert across the delegate/revoke pair.
fn count_and_find(b: NodeId, label: Label, target: NodeId) -> (usize, bool) {
    let edges = match list_edges(b) {
        Ok(v) => v,
        Err(e) => {
            println!("  list_edges({}): {} — bailing", b, e);
            exit(10);
        }
    };
    let mut found = false;
    for e in &edges {
        if e.label == label && e.target == target {
            found = true;
            break;
        }
    }
    (edges.len(), found)
}

fn main() {
    let (a0, a1) = args();
    if a0 == 0 || a1 == 0 {
        println!("cdtsmoke-α: usage — `spawn cdtsmoke-α` (kernel provides T + B)");
        exit(2);
    }
    let t = NodeId(a0 as u64);
    let b = NodeId(a1 as u64);

    println!("cdtsmoke-α: T={} B={}", t, b);

    // Step 1: baseline.
    let (baseline, has_write_to_t_pre) = count_and_find(b, Label::Write, t);
    println!("  baseline: B has {} live edge(s)", baseline);
    if has_write_to_t_pre {
        println!("cdtsmoke-α: FAIL — B already had write→{} at baseline", t);
        exit(11);
    }

    // Step 2: delegate.
    let new_idx = match delegate_edge(t, b, "write") {
        Ok(idx) => idx,
        Err(Errno::Perm) => {
            println!("cdtsmoke-α: FAIL — delegate returned EPERM (no grant on {} or no write edge?)", t);
            exit(12);
        }
        Err(Errno::NotFound) => {
            println!("cdtsmoke-α: FAIL — delegate returned ENOENT ({} or {} missing)", t, b);
            exit(13);
        }
        Err(e) => {
            println!("cdtsmoke-α: FAIL — delegate returned {}", e);
            exit(14);
        }
    };
    println!("  delegate write→B: ok (idx={})", new_idx);

    // Step 3: confirm B has the derived edge.
    let (after_d, has_write_to_t_post) = count_and_find(b, Label::Write, t);
    if after_d != baseline + 1 || !has_write_to_t_post {
        println!(
            "cdtsmoke-α: FAIL — after delegate, B has {} edge(s) and write→{}: {} \
             (wanted {} and true)",
            after_d, t, has_write_to_t_post, baseline + 1,
        );
        exit(15);
    }
    println!(
        "  after delegate: B has {} live edge(s) (+1 write→{} ✓)",
        after_d, t,
    );

    // Step 4: revoke own write→T. Cascade should hit B's derived edge.
    let cascade = match revoke_edge(t, "write") {
        Ok(n) => n,
        Err(e) => {
            println!("cdtsmoke-α: FAIL — revoke returned {}", e);
            exit(16);
        }
    };
    if cascade != 2 {
        println!(
            "cdtsmoke-α: FAIL — revoke cascade tombstoned {} edge(s), wanted 2 (own + derived)",
            cascade,
        );
        exit(17);
    }
    println!("  revoke write@{}: ok ({} edge(s) tombstoned)", t, cascade);

    // Step 5: confirm B's derived edge is gone.
    let (after_r, still_has_post_revoke) = count_and_find(b, Label::Write, t);
    if after_r != baseline || still_has_post_revoke {
        println!(
            "cdtsmoke-α: FAIL — after revoke, B has {} edge(s) and write→{}: {} \
             (wanted {} and false)",
            after_r, t, still_has_post_revoke, baseline,
        );
        exit(18);
    }
    println!(
        "  after revoke: B has {} live edge(s) (derived edge gone ✓)",
        after_r,
    );

    println!("cdtsmoke-α: PASS");
}
