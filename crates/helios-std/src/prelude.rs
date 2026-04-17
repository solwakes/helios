//! Glob-import-friendly names for Helios user programs.
//!
//! `use helios_std::prelude::*;` pulls in the day-to-day surface:
//! graph primitives, `Errno`, heap-backed collections, and the
//! `print!`/`println!` macros (via the `helios_std` crate alias).
//!
//! Example:
//!
//! ```ignore
//! #![no_std]
//! #![no_main]
//! extern crate alloc;
//!
//! use helios_std::prelude::*;
//!
//! helios_std::helios_entry!(main);
//!
//! fn main() {
//!     let me: NodeId = self_id();
//!     helios_std::println!("hello from #{}", me);
//! }
//! ```
//!
//! The `print!` / `println!` macros aren't re-exported here — they're
//! already `#[macro_export]`, so `helios_std::println!(...)` works
//! from any module.

pub use crate::graph::{
    follow_edge, list_edges, list_edges_into, read_node, write_node, Edge, EdgeInfo, Errno,
    Label, LabelKind, NodeId,
};
pub use crate::io::{print, println};
pub use crate::task::{args, exit, self_id};

// Alloc re-exports so user code can say `Vec`, `String`, `Box`, `format!`
// straight out of the prelude.
pub use alloc::boxed::Box;
pub use alloc::format;
pub use alloc::string::{String, ToString};
pub use alloc::vec;
pub use alloc::vec::Vec;
