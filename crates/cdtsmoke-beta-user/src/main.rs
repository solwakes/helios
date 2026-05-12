//! `cdtsmoke-beta-user` — CDT (M35) litmus β.
//!
//! Exercises **task-exit cascade**: A delegates write→B, then exits
//! *without* revoking. The kernel's post-exit cleanup walks A's
//! outgoing edges and `cascade_tombstone`s each one, which transitively
//! kills the derived edge on B. The β invariant — *caps don't outlive
//! the principal that granted them* — is observable in the kernel
//! console as the `[user] task #N exit: cascade-tombstoned M edge(s)
//! via CDT` line, which on a normal task-exit *without* delegations is
//! emitted only when the cascade did something. Companion to
//! `cdtsmoke-alpha-user` (delegate+revoke within A).
//!
//! # Choreography (β — this binary)
//!
//! Single-hart cooperative scheduler. A runs end-to-end; B never
//! executes. We use B's task node as the recipient of a delegated
//! cap. Observable from inside A: post-delegate, `list_edges(B)`
//! shows the derived edge. Observable from the kernel-console after
//! A exits: a `cascade-tombstoned M edge(s) via CDT` line that
//! reflects A's root edges *plus* the one derived edge on B.
//!
//! 1. List B's edges. Note `baseline` count.
//! 2. Call `delegate_edge(T, B, "write")`. Expect `Ok(_)`.
//! 3. List B's edges again. Expect `baseline + 1`; new edge has
//!    `target == T` and `label == Label::Write`.
//! 4. Exit cleanly with code 0 — *no revoke*. The kernel's task-exit
//!    cleanup must cascade-tombstone the derived edge.
//!
//! The β assertion ("B's derived edge tombstoned at exit") is verified
//! by the kernel-console cascade-count line, which a human (or future
//! parser) cross-checks against α's revoke count + 1 worth of
//! additional root edges.
//!
//! # Cap model
//!
//! Identical to α (see that crate). Args: `a0 = T_id`, `a1 =
//! B_task_node_id`.
//!
//! # Output shape
//!
//! ```text
//! cdtsmoke-β: T=#101 B=#102
//!   baseline: B has N live edge(s)
//!   delegate write→B: ok (idx=...)
//!   after delegate: B has N+1 live edge(s) (+1 write→#101 ✓)
//! cdtsmoke-β: DELEGATED — exiting without revoke; watch kernel for cascade
//! ```
//!
//! Then, in kernel console after task exit:
//!
//! ```text
//! [user] task #N exit: cascade-tombstoned M edge(s) via CDT
//! ```
//!
//! Where M includes A's root edges + 1 (the derived edge on B).

#![no_std]
#![no_main]

extern crate alloc;

use helios_std::prelude::*;

helios_std::helios_entry!(main);

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
        println!("cdtsmoke-β: usage — `spawn cdtsmoke-β` (kernel provides T + B)");
        exit(2);
    }
    let t = NodeId(a0 as u64);
    let b = NodeId(a1 as u64);

    println!("cdtsmoke-β: T={} B={}", t, b);

    // Step 1: baseline.
    let (baseline, has_write_to_t_pre) = count_and_find(b, Label::Write, t);
    println!("  baseline: B has {} live edge(s)", baseline);
    if has_write_to_t_pre {
        println!("cdtsmoke-β: FAIL — B already had write→{} at baseline", t);
        exit(11);
    }

    // Step 2: delegate.
    let new_idx = match delegate_edge(t, b, "write") {
        Ok(idx) => idx,
        Err(Errno::Perm) => {
            println!("cdtsmoke-β: FAIL — delegate returned EPERM");
            exit(12);
        }
        Err(Errno::NotFound) => {
            println!("cdtsmoke-β: FAIL — delegate returned ENOENT");
            exit(13);
        }
        Err(e) => {
            println!("cdtsmoke-β: FAIL — delegate returned {}", e);
            exit(14);
        }
    };
    println!("  delegate write→B: ok (idx={})", new_idx);

    // Step 3: confirm B has the derived edge.
    let (after_d, has_write_to_t_post) = count_and_find(b, Label::Write, t);
    if after_d != baseline + 1 || !has_write_to_t_post {
        println!(
            "cdtsmoke-β: FAIL — after delegate, B has {} edge(s) and write→{}: {} \
             (wanted {} and true)",
            after_d, t, has_write_to_t_post, baseline + 1,
        );
        exit(15);
    }
    println!(
        "  after delegate: B has {} live edge(s) (+1 write→{} ✓)",
        after_d, t,
    );

    // Step 4: exit without revoke. Task-exit cleanup must
    // cascade-tombstone the derived edge.
    println!(
        "cdtsmoke-β: DELEGATED — exiting without revoke; watch kernel for cascade"
    );
}
