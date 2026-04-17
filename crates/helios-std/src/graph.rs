//! Typed graph primitives that mirror the kernel's graph model.
//!
//! This module is the Helios equivalent of `std::fs` — but where
//! `std::fs` talks about files with paths, `helios-std` talks about
//! *nodes* identified by [`NodeId`], reached via *edges* labelled with
//! capabilities. A task can only touch the nodes its outgoing edges
//! reach.

use alloc::vec::Vec;

use crate::sys;

// ---------------------------------------------------------------------------
// NodeId and Label
// ---------------------------------------------------------------------------

/// A handle to a node in the Helios graph. Opaque 64-bit id.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub u64);

impl core::fmt::Debug for NodeId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "NodeId({})", self.0)
    }
}

impl core::fmt::Display for NodeId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// The label on an edge. In Helios, edge labels *are* capability
/// tokens: having an edge labelled `Read` to a node grants read
/// access; having an edge labelled `Traverse` grants the right to
/// enumerate the node's own edges via syscall.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Label {
    /// `read` — R-only MMU mapping; grants `SYS_READ_NODE`.
    Read,
    /// `write` — R+W MMU mapping; grants `SYS_WRITE_NODE` (implies read).
    Write,
    /// `exec` — R+X (currently R+W+X; see kernel `build_user_address_space`
    /// for the M31 W^X trade-off) MMU mapping for code pages.
    Exec,
    /// `traverse` — no MMU mapping; grants `SYS_LIST_EDGES` /
    /// `SYS_FOLLOW_EDGE` on the target node.
    Traverse,
    /// Any other edge kind the kernel reports. Includes structural
    /// edges like `child`/`parent` which aren't capability labels.
    Unknown(u8),
}

impl Label {
    /// Decode the kind byte returned by `SYS_LIST_EDGES`.
    ///
    /// ABI: 0 = unknown, 1 = read, 2 = write, 3 = exec, 4 = traverse.
    pub fn from_kind(kind: u8) -> Self {
        match kind {
            1 => Label::Read,
            2 => Label::Write,
            3 => Label::Exec,
            4 => Label::Traverse,
            other => Label::Unknown(other),
        }
    }

    /// Kernel's label-kind byte for this variant.
    pub fn as_kind(self) -> u8 {
        match self {
            Label::Read => 1,
            Label::Write => 2,
            Label::Exec => 3,
            Label::Traverse => 4,
            Label::Unknown(b) => b,
        }
    }

    /// The canonical string name for this label (also what the kernel
    /// stores in the graph).
    pub fn as_str(self) -> &'static str {
        match self {
            Label::Read => "read",
            Label::Write => "write",
            Label::Exec => "exec",
            Label::Traverse => "traverse",
            Label::Unknown(_) => "?",
        }
    }
}

impl core::fmt::Display for Label {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One outgoing edge: where it points and what access it grants.
#[derive(Clone, Copy, Debug)]
pub struct EdgeInfo {
    pub target: NodeId,
    pub label: Label,
}

impl Default for EdgeInfo {
    fn default() -> Self {
        Self { target: NodeId(0), label: Label::Unknown(0) }
    }
}

/// Alias for [`EdgeInfo`] matching the name used in the M31 design doc
/// (`docs/userspace/rust-std.md`), where the struct is referred to as
/// simply `Edge`. The two names are interchangeable.
pub type Edge = EdgeInfo;

/// Alias for [`Label`]. `rust-std.md` calls the enum `LabelKind`; the
/// shorter `Label` is the preferred spelling inside this crate.
pub type LabelKind = Label;

// ---------------------------------------------------------------------------
// Errno
// ---------------------------------------------------------------------------

/// Typed view of the negative error codes Helios syscalls return.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Errno {
    /// `-EPERM` — capability check failed.
    Perm,
    /// `-ENOENT` — node or edge not found.
    NotFound,
    /// `-EINVAL` — bad argument (out-of-range pointer, too-long string, etc.).
    Invalid,
    /// `-ENOMEM` — no backing frames or no contiguous VA slots available
    /// (M33: [`map_node`] can return this when the task's data window
    /// is fragmented or full).
    NoMem,
    /// Any other negative return not covered above.
    Other(isize),
}

impl Errno {
    /// Decode a raw syscall return (only call with negative values).
    pub fn from_raw(r: isize) -> Self {
        match r {
            sys::EPERM => Errno::Perm,
            sys::ENOENT => Errno::NotFound,
            sys::EINVAL => Errno::Invalid,
            sys::ENOMEM => Errno::NoMem,
            other => Errno::Other(other),
        }
    }
}

impl core::fmt::Display for Errno {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Errno::Perm => f.write_str("EPERM"),
            Errno::NotFound => f.write_str("ENOENT"),
            Errno::Invalid => f.write_str("EINVAL"),
            Errno::NoMem => f.write_str("ENOMEM"),
            Errno::Other(v) => write!(f, "E({})", v),
        }
    }
}

// ---------------------------------------------------------------------------
// Typed wrappers around the graph-y syscalls
// ---------------------------------------------------------------------------

/// Read up to `buf.len()` bytes of the target node's content into
/// `buf`. Returns the number of bytes actually read.
///
/// Requires a `read` or `write` edge from the caller to `id`.
pub fn read_node(id: NodeId, buf: &mut [u8]) -> Result<usize, Errno> {
    let r = unsafe {
        sys::syscall3(
            sys::SYS_READ_NODE,
            id.0 as usize,
            buf.as_mut_ptr() as usize,
            buf.len(),
        )
    };
    if r < 0 {
        Err(Errno::from_raw(r))
    } else {
        Ok(r as usize)
    }
}

/// Overwrite `id`'s content with `buf`. Returns bytes written.
///
/// Requires a `write` edge from the caller to `id`. Write is
/// whole-content replace (not append) in M30/M31.
pub fn write_node(id: NodeId, buf: &[u8]) -> Result<usize, Errno> {
    let r = unsafe {
        sys::syscall3(
            sys::SYS_WRITE_NODE,
            id.0 as usize,
            buf.as_ptr() as usize,
            buf.len(),
        )
    };
    if r < 0 {
        Err(Errno::from_raw(r))
    } else {
        Ok(r as usize)
    }
}

/// Number of edges staged per `SYS_LIST_EDGES` call. Keeps stack use
/// bounded; per-call ceiling until a paging/offset variant lands.
const LIST_EDGES_STAGE: usize = 32;

/// Bytes per edge entry on the wire (matches kernel ABI).
const EDGE_ENTRY_SIZE: usize = 16;

/// Enumerate up to `out.len()` outgoing edges of `src` into `out`.
/// Returns the number of entries written (which may be less than the
/// total edge count, if `out` is smaller).
///
/// This is the zero-allocation variant — useful inside allocator code-
/// paths or for fixed upper bounds. See [`list_edges`] for the more
/// ergonomic `Vec`-returning form.
///
/// Requires a `traverse` edge from the caller to `src`. To introspect
/// the caller's *own* edges, the task needs a `traverse` edge back to
/// itself (the kernel adds this at spawn time when `self_traverse =
/// true`).
pub fn list_edges_into(src: NodeId, out: &mut [EdgeInfo]) -> Result<usize, Errno> {
    if out.is_empty() {
        return Ok(0);
    }
    // The kernel writes 16 bytes per entry (u64 target, u8 kind, 7
    // pad). Stage into a raw byte buffer on the stack so we don't
    // depend on EdgeInfo's in-memory layout. The buffer must live in
    // user-mapped memory (stack is fine).
    let mut stage = [0u8; EDGE_ENTRY_SIZE * LIST_EDGES_STAGE];
    let n = core::cmp::min(out.len(), LIST_EDGES_STAGE);
    let r = unsafe {
        sys::syscall3(
            sys::SYS_LIST_EDGES,
            src.0 as usize,
            stage.as_mut_ptr() as usize,
            n,
        )
    };
    if r < 0 {
        return Err(Errno::from_raw(r));
    }
    let count = r as usize;
    for i in 0..count {
        let base = i * EDGE_ENTRY_SIZE;
        let mut id_bytes = [0u8; 8];
        id_bytes.copy_from_slice(&stage[base..base + 8]);
        let target = NodeId(u64::from_le_bytes(id_bytes));
        let label = Label::from_kind(stage[base + 8]);
        out[i] = EdgeInfo { target, label };
    }
    Ok(count)
}

/// Enumerate the outgoing edges of `src` as a fresh [`Vec`].
///
/// This is the allocating — and usually more ergonomic — variant.
/// Internally stages [`LIST_EDGES_STAGE`] entries at a time; the
/// kernel's current `SYS_LIST_EDGES` returns edges in graph order up
/// to the requested max. If a node has more edges than the stage size,
/// the tail is not visible via this call (tracked: an offset-aware
/// variant is part of the next syscall-ABI pass).
///
/// Requires a `traverse` edge from the caller to `src`.
pub fn list_edges(src: NodeId) -> Result<Vec<EdgeInfo>, Errno> {
    let mut stage: [EdgeInfo; LIST_EDGES_STAGE] =
        [EdgeInfo { target: NodeId(0), label: Label::Unknown(0) }; LIST_EDGES_STAGE];
    let n = list_edges_into(src, &mut stage)?;
    let mut out = Vec::with_capacity(n);
    for e in stage.iter().take(n) {
        out.push(*e);
    }
    Ok(out)
}

/// Find the first outgoing edge from `src` whose label matches
/// `label`, and return its target. Typically `label` is one of
/// `"child"`, `"parent"`, `"read"`, `"write"`, `"exec"`, `"traverse"`,
/// or any other string the graph uses.
///
/// Requires a `traverse` edge from the caller to `src`.
pub fn follow_edge(src: NodeId, label: &str) -> Result<NodeId, Errno> {
    let r = unsafe {
        sys::syscall3(
            sys::SYS_FOLLOW_EDGE,
            src.0 as usize,
            label.as_ptr() as usize,
            label.len(),
        )
    };
    if r < 0 {
        Err(Errno::from_raw(r))
    } else {
        Ok(NodeId(r as u64))
    }
}

// ---------------------------------------------------------------------------
// M33: SYS_MAP_NODE — kernel-granted anonymous writable memory.
// ---------------------------------------------------------------------------

/// Ask the kernel for a fresh, zeroed writable memory region of at
/// least `size` bytes.
///
/// On success, returns a non-null pointer to the first byte of the new
/// region. The kernel:
///
/// - Rounds `size` up to a 4 KiB multiple.
/// - Creates a new `Memory` node in the graph.
/// - Allocates the backing frames.
/// - Adds a `write` edge from the calling task to the new node, which
///   under the Helios cap semantics auto-implies `read` as well.
/// - Maps the frames into the task's data-VA window as R+W+U leaves.
///
/// Returns:
///
/// - `Err(Errno::Invalid)` for `size == 0` or a request bigger than
///   the task's data window can hold (16 pages = 64 KiB in M33).
/// - `Err(Errno::NoMem)` when the task's data window doesn't have a
///   contiguous run of free slots for the request.
///
/// # Safety note
///
/// The returned pointer is valid for the lifetime of the current task
/// — the mapped region dies when the task exits (the kernel removes
/// the `Memory` node, frees the task→mem edge, and frees the page
/// tables). There is no [`unmap_node`] in M33; per-allocation free is
/// a future milestone.
///
/// [`unmap_node`]: # "planned SYS_UNMAP_NODE, not yet shipped"
pub fn map_node(size: usize) -> Result<core::ptr::NonNull<u8>, Errno> {
    let r = unsafe { sys::sys_map_node(size, 0) };
    if r < 0 {
        return Err(Errno::from_raw(r));
    }
    // SAFETY: kernel returns either a negative errno (handled above)
    // or a positive VA in the user data window, which by construction
    // is non-null.
    Ok(unsafe { core::ptr::NonNull::new_unchecked(r as *mut u8) })
}

/// Like [`map_node`] but returns the whole region as a borrowed mutable
/// byte slice. The slice's length is `size` rounded up to the next 4 KiB
/// multiple — i.e. the actual kernel-backed footprint.
///
/// The lifetime is `'static` because the allocation outlives any
/// reasonable caller: it's released when the task exits (see
/// [`map_node`] for the ownership story). Holding two `&'static mut`
/// slices to *overlapping* regions would be unsound, but [`map_node`]
/// + [`map_node_slice`] never hand out overlapping regions — each
/// call gets its own disjoint slot range.
pub fn map_node_slice(size: usize) -> Result<&'static mut [u8], Errno> {
    let ptr = map_node(size)?;
    // Round up to 4 KiB — matches the kernel's allocation granularity.
    let pages = (size + 4095) / 4096;
    let total = pages * 4096;
    // SAFETY: `ptr` is non-null, 4 KiB-aligned, writable from U-mode
    // (the kernel installed R+W+U leaves), and `total` <= 64 KiB so
    // the arithmetic doesn't overflow on RV64.
    Ok(unsafe { core::slice::from_raw_parts_mut(ptr.as_ptr(), total) })
}
