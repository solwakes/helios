//! `gfollow-user` — graph-native edge follow (post-M34 utility).
//!
//! `spawn gfollow <src> <label>` returns the target of the first
//! outgoing edge from `<src>` whose label matches `<label>`. Thin
//! wrapper over `SYS_FOLLOW_EDGE` — the smallest interesting program
//! that exercises the typed `follow_edge` API in helios-std and
//! demonstrates a clean error-path output for the three common
//! capability/lookup failures.
//!
//! # Cap model
//!
//! `gfollow` needs:
//!   - a `traverse` cap on `<src>` (so `SYS_FOLLOW_EDGE` succeeds), and
//!   - a `read` cap on the kernel-managed label-buffer node, which the
//!     shell stuffs with the label bytes at spawn time and passes via
//!     `a1`.
//!
//! Both caps are pre-granted by `cmd_spawn` in the shell. The user
//! task itself is unprivileged.
//!
//! # Output
//!
//! ```text
//! gfollow #1 "child"
//!   -> #2
//! ```
//!
//! On miss / EPERM / bad node, prints a one-line error and exits with a
//! non-zero status. No panic, no kill — capability and lookup failures
//! are recoverable at the typed-syscall layer.
//!
//! # Why pass the label through a node?
//!
//! `follow_edge` needs a `&str`, but the kernel's user-mode argument
//! window is two scalar registers (a0, a1). The shell stuffs the label
//! bytes into a long-lived "gfollow-label-buf" Text node and grants
//! this task a `read` cap on it; we then pull the label out via
//! `read_node`. Reusing one buf node across spawns is safe because
//! `cmd_spawn` is synchronous — the shell blocks in `run_user_task_*`
//! until the task exits before processing the next command, so the
//! label can't be clobbered out from under us.

#![no_std]
#![no_main]

extern crate alloc;

use helios_std::prelude::*;

helios_std::helios_entry!(main);

/// Per-call read buffer for the label string. 256 B comfortably covers
/// every label string used in Helios today (`child`, `parent`, `self`,
/// the four cap-edge labels, plus any custom labels users introduce).
/// The kernel's `SYS_READ_NODE` truncates to `buf.len()`, so an
/// over-long label would read in clipped — but no such label exists.
const LABEL_BUF_LEN: usize = 256;

fn main() {
    // a0 = source node id; a1 = node id of the kernel-managed label
    // buffer (its content holds the label bytes).
    let (a0, a1) = args();

    if a0 == 0 || a1 == 0 {
        println!("gfollow: usage — `spawn gfollow <src> <label>`");
        exit(2);
    }

    let src = NodeId(a0 as u64);
    let label_buf = NodeId(a1 as u64);

    // Pull the label string out of the buffer node. We allocate on the
    // heap rather than the stack: helios-std user tasks have a single-
    // page stack and a 256 B local would be fine today, but using the
    // heap keeps us under the same constraint as cat-user without
    // having to think about stack ceiling.
    let mut buf: Vec<u8> = vec![0u8; LABEL_BUF_LEN];
    let n = match read_node(label_buf, &mut buf[..]) {
        Ok(n) => n,
        Err(Errno::Perm) => {
            println!(
                "gfollow: EPERM reading label buffer #{} — shell grant missing?",
                label_buf,
            );
            exit(1);
        }
        Err(Errno::NotFound) => {
            println!(
                "gfollow: ENOENT — label buffer #{} does not exist",
                label_buf,
            );
            exit(1);
        }
        Err(e) => {
            println!("gfollow: read_node({}) failed: {}", label_buf, e);
            exit(1);
        }
    };

    // Trim any trailing whitespace (the shell shouldn't add any, but
    // defensively don't pass `"child\n"` to follow_edge — the kernel
    // would not match it against a stored `"child"`).
    let raw = &buf[..n];
    let mut end = raw.len();
    while end > 0 {
        let b = raw[end - 1];
        if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' || b == 0 {
            end -= 1;
        } else {
            break;
        }
    }
    let trimmed = &raw[..end];

    let label = match core::str::from_utf8(trimmed) {
        Ok(s) => s,
        Err(_) => {
            println!("gfollow: label buffer is not valid UTF-8");
            exit(1);
        }
    };

    if label.is_empty() {
        println!("gfollow: empty label");
        exit(2);
    }

    println!("gfollow {} \"{}\"", src, label);

    match follow_edge(src, label) {
        Ok(target) => {
            println!("  -> {}", target);
        }
        Err(Errno::Perm) => {
            println!("  EPERM (no traverse cap to {})", src);
            exit(1);
        }
        Err(Errno::NotFound) => {
            // Two flavours of NotFound at this point: the source node
            // itself doesn't exist, or no edge with that label was
            // found. The kernel returns the same errno for both, so
            // we report the more useful framing — caller knows their
            // input.
            println!("  no edge \"{}\" from {}", label, src);
            exit(2);
        }
        Err(other) => {
            println!("  {}", other);
            exit(3);
        }
    }
}
