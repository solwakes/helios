//! `cat-user` — a graph-native `cat` for Helios (M32).
//!
//! Reads a node's content and prints it to the UART. This is the M32
//! litmus test for `helios-std`: does the typed syscall API make a
//! normal tool feel like normal Rust?
//!
//! Answer (per this file): yes — a few lines of `match` over
//! `Result<usize, Errno>` and you're done. No POSIX-shaped fiction,
//! no fds, no paths. You get a `NodeId` handed to you, you call
//! `read_node`, you print.
//!
//! ## Usage
//!
//! From the kernel shell:
//!
//! ```text
//! helios> spawn cat <node_id>
//! ```
//!
//! The shell command creates a fresh task with:
//!   - an `exec` edge to the `cat-user` code binary
//!   - a `read` edge to `<node_id>` so `SYS_READ_NODE` succeeds
//!   - `a0 = <node_id>` so this program knows what to read
//!
//! Run without a numeric arg (a0 == 0) → we refuse to proceed.
//! Run on a node without a read edge → `read_node` returns
//! `Err(Errno::Perm)` and we print it. No panic, no task kill: a
//! capability violation on a typed syscall is a recoverable error.

#![no_std]
#![no_main]

extern crate alloc;

use helios_std::prelude::*;

helios_std::helios_entry!(main);

/// Per-call read buffer. 4 KiB covers every node we expect to read
/// in M32 (kernel's SYS_READ_NODE reads from offset 0 up to `buf.len()`
/// bytes — no offset variant yet, so longer nodes get truncated). We
/// allocate this on the heap (via `Vec::with_capacity(READ_BUF_LEN)`)
/// rather than as a stack array, because the user task's stack is
/// exactly one 4 KiB page: a 4 KiB stack-local array blows past the
/// frame pointer into unmapped VA and page-faults. Heap lives in
/// helios-std's bump arena, which is inside the R/W/X mapping of the
/// binary image itself.
const READ_BUF_LEN: usize = 4096;

fn main() {
    let (a0, _a1) = helios_std::task::args();

    if a0 == 0 {
        helios_std::println!(
            "cat: usage — `spawn cat <node_id>` (no node given via a0)"
        );
        exit(2);
    }

    let target = NodeId(a0 as u64);
    let me = self_id();
    helios_std::println!(
        "cat: task {} reading node {}",
        me, target,
    );

    // Heap-allocated so we don't overflow the single-page user stack.
    let mut buf: Vec<u8> = vec![0u8; READ_BUF_LEN];
    match read_node(target, &mut buf[..]) {
        Ok(0) => {
            helios_std::println!("(empty)");
        }
        Ok(n) => {
            // Best-effort text: lossy UTF-8 so a binary node still
            // prints *something* readable, with replacement chars
            // for invalid sequences rather than panicking.
            let slice = &buf[..n];
            let text = core::str::from_utf8(slice).unwrap_or_else(|_| {
                // Fallback: manually filter to ASCII printable + \n /
                // \t. Anything else becomes '?'.
                // Rather than alloc a new String here, print in
                // place: chunk by chunk.
                let mut cursor = 0usize;
                let mut scratch = [0u8; 256];
                while cursor < slice.len() {
                    let take = core::cmp::min(scratch.len(), slice.len() - cursor);
                    for i in 0..take {
                        let b = slice[cursor + i];
                        scratch[i] = if b == b'\n' || b == b'\t' || (0x20..=0x7e).contains(&b) {
                            b
                        } else {
                            b'?'
                        };
                    }
                    // SAFETY: scratch contains only ASCII.
                    let s = unsafe { core::str::from_utf8_unchecked(&scratch[..take]) };
                    helios_std::io::print(s);
                    cursor += take;
                }
                // We've already printed; return empty so the caller
                // doesn't re-print.
                ""
            });
            if !text.is_empty() {
                helios_std::io::print(text);
            }
            // Ensure trailing newline so the shell prompt starts on a
            // fresh line whether or not the node had one.
            if !slice.ends_with(b"\n") {
                helios_std::println!();
            }
            helios_std::println!(
                "cat: {} byte{} read from {}",
                n,
                if n == 1 { "" } else { "s" },
                target,
            );
        }
        Err(Errno::Perm) => {
            helios_std::println!(
                "cat: EPERM reading {} — task has no `read` cap for this node",
                target,
            );
            exit(1);
        }
        Err(Errno::NotFound) => {
            helios_std::println!("cat: ENOENT — node {} does not exist", target);
            exit(1);
        }
        Err(e) => {
            helios_std::println!("cat: read_node({}) failed with {}", target, e);
            exit(1);
        }
    }
}
