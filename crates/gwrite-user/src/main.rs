//! `gwrite-user` — graph-native node-content overwrite (post-M34 utility).
//!
//! `spawn gwrite <id> <content>` overwrites `<id>`'s content with the
//! given byte string and prints the count written. Thin wrapper over
//! `SYS_WRITE_NODE` — completes the first-pass utility set
//! (list / read / recurse / step / write) and demonstrates a clean
//! error path for the EPERM and NotFound cases on writes.
//!
//! # Cap model
//!
//! `gwrite` needs:
//!   - a `write` cap on `<id>` (so `SYS_WRITE_NODE` succeeds), and
//!   - a `read` cap on the kernel-managed content-buffer node, which
//!     the shell stuffs with the content bytes at spawn time and
//!     passes via `a1`.
//!
//! Both caps are pre-granted by `cmd_spawn` in the shell. The user
//! task itself is unprivileged.
//!
//! # Output
//!
//! ```text
//! gwrite #5 (12 bytes)
//!   -> wrote 12 bytes
//! ```
//!
//! On EPERM / ENOENT / bad arg, prints a one-line error and exits with
//! a non-zero status. No panic, no kill — capability and lookup
//! failures are recoverable at the typed-syscall layer.
//!
//! # Why pass the content through a node?
//!
//! `write_node` needs a `&[u8]`, but the kernel's user-mode argument
//! window is two scalar registers (a0, a1). The shell stuffs the
//! content bytes into a long-lived "gwrite-content-buf" Text node and
//! grants this task a `read` cap on it; we then pull the content out
//! via `read_node`. Reusing one buf node across spawns is safe because
//! `cmd_spawn` is synchronous — the shell blocks in
//! `run_user_task_with_caps` until the task exits before processing
//! the next command, so the buffer can't be clobbered out from under
//! us.
//!
//! # Buffer ceiling
//!
//! The buffer is sized at 4 KiB (one page). The shell-side rewrite
//! truncates if the user types a longer line; the kernel's
//! `SYS_WRITE_NODE` would in turn cap by page granularity. 4 KiB is
//! comfortably more than any interactive shell line (the kernel's
//! console line buffer is itself smaller).

#![no_std]
#![no_main]

extern crate alloc;

use helios_std::prelude::*;

helios_std::helios_entry!(main);

/// Per-call read buffer for the content bytes. 4 KiB matches one page
/// — the kernel's per-task shell line buffer is well under this, and
/// `SYS_READ_NODE` truncates to `buf.len()` so over-long content
/// reads in clipped without faulting.
const CONTENT_BUF_LEN: usize = 4096;

fn main() {
    // a0 = target node id; a1 = node id of the kernel-managed content
    // buffer (its content holds the bytes to write).
    let (a0, a1) = args();

    if a0 == 0 || a1 == 0 {
        println!("gwrite: usage — `spawn gwrite <id> <content>`");
        exit(2);
    }

    let target = NodeId(a0 as u64);
    let content_buf = NodeId(a1 as u64);

    // Pull the content bytes out of the buffer node. Heap-allocate
    // rather than stack-allocate — same reasoning as gfollow: user
    // tasks have a single-page stack and a 4 KiB local would consume
    // most of it.
    let mut buf: Vec<u8> = vec![0u8; CONTENT_BUF_LEN];
    let n = match read_node(content_buf, &mut buf[..]) {
        Ok(n) => n,
        Err(Errno::Perm) => {
            println!(
                "gwrite: EPERM reading content buffer #{} — shell grant missing?",
                content_buf,
            );
            exit(1);
        }
        Err(Errno::NotFound) => {
            println!(
                "gwrite: ENOENT — content buffer #{} does not exist",
                content_buf,
            );
            exit(1);
        }
        Err(e) => {
            println!("gwrite: read_node({}) failed: {}", content_buf, e);
            exit(1);
        }
    };

    let content = &buf[..n];

    println!("gwrite {} ({} byte{})", target, n, if n == 1 { "" } else { "s" });

    match write_node(target, content) {
        Ok(written) => {
            println!("  -> wrote {} byte{}", written, if written == 1 { "" } else { "s" });
        }
        Err(Errno::Perm) => {
            println!("  EPERM (no write cap to {})", target);
            exit(1);
        }
        Err(Errno::NotFound) => {
            println!("  ENOENT — target {} does not exist", target);
            exit(2);
        }
        Err(other) => {
            println!("  {}", other);
            exit(3);
        }
    }
}
