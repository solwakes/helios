//! Glob-import-friendly names for Helios user programs.
//!
//! `use helios_std::prelude::*;` pulls in the day-to-day surface:
//! graph primitives, `Errno`, heap-backed collections, and the
//! `print!` / `println!` macros.
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
//!     println!("hello from {}", me);
//! }
//! ```
//!
//! # Name clash note
//!
//! helios-std has both `io::print` / `io::println` **functions** (take
//! `&str`) and `print!` / `println!` **macros** (format their
//! arguments). We re-export the macros here — they share the Rust
//! macro namespace and user code written against `std` habits
//! (`println!(fmt, ...)`) just works. The functions are still
//! reachable as `helios_std::io::print` if you want zero-format str
//! output.

pub use crate::graph::{
    follow_edge, list_edges, list_edges_into, map_node, map_node_slice, read_node, write_node,
    Edge, EdgeInfo, Errno, Label, LabelKind, NodeId,
};
pub use crate::io::Stdout;
pub use crate::task::{args, exit, self_id};

// Re-export the format-aware macros. They're `#[macro_export]`'d in
// `io.rs`, which means they live at `helios_std::<name>` — `pub use`
// brings them into the prelude's scope. `print!` / `println!` then
// look like their std counterparts to user code.
pub use crate::{print, println};

// Alloc re-exports so user code can say `Vec`, `String`, `Box`, `format!`
// straight out of the prelude.
pub use alloc::boxed::Box;
pub use alloc::format;
pub use alloc::string::{String, ToString};
pub use alloc::vec;
pub use alloc::vec::Vec;
