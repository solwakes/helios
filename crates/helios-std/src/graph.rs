//! Typed graph primitives that mirror the kernel's graph model.
//!
//! This module is the Helios equivalent of `std::fs` — but where
//! `std::fs` talks about files with paths, `helios-std` talks about
//! *nodes* identified by [`NodeId`], reached via *edges* labelled with
//! capabilities. A task can only touch the nodes its outgoing edges
//! reach.

use alloc::string::String;
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
    /// `grant` (M35) — no MMU mapping; authorises `SYS_DELEGATE_EDGE`
    /// on the target node. Without `grant`, holding a cap does not
    /// imply the right to redistribute it.
    Grant,
    /// Any other edge kind the kernel reports. Includes structural
    /// edges like `child`/`parent` which aren't capability labels.
    Unknown(u8),
}

impl Label {
    /// Decode the kind byte returned by `SYS_LIST_EDGES`.
    ///
    /// ABI: 0 = unknown, 1 = read, 2 = write, 3 = exec, 4 = traverse,
    /// 5 = grant.
    pub fn from_kind(kind: u8) -> Self {
        match kind {
            1 => Label::Read,
            2 => Label::Write,
            3 => Label::Exec,
            4 => Label::Traverse,
            5 => Label::Grant,
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
            Label::Grant => 5,
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
            Label::Grant => "grant",
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
pub(crate) const EDGE_ENTRY_SIZE: usize = 16;

/// Decode a single 16-byte edge entry from the kernel wire format.
///
/// The kernel writes one edge as: `u64 target_id` (little-endian, 8
/// bytes), `u8 label_kind`, then 7 padding bytes. This matches
/// `EdgeInfo` in fields but not in in-memory layout, so callers must
/// stage into a raw byte buffer and decode through this helper.
///
/// Made `pub(crate)` rather than private so the host-side unit tests
/// in this module's `#[cfg(test)] mod tests` can exercise it without
/// going through the syscall boundary.
#[inline]
pub(crate) fn decode_edge_entry(entry: &[u8; EDGE_ENTRY_SIZE]) -> EdgeInfo {
    let mut id_bytes = [0u8; 8];
    id_bytes.copy_from_slice(&entry[0..8]);
    let target = NodeId(u64::from_le_bytes(id_bytes));
    let label = Label::from_kind(entry[8]);
    EdgeInfo { target, label }
}

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
        let mut entry = [0u8; EDGE_ENTRY_SIZE];
        entry.copy_from_slice(&stage[base..base + EDGE_ENTRY_SIZE]);
        out[i] = decode_edge_entry(&entry);
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
// M34: SYS_READ_EDGE_LABEL — full UTF-8 label for an edge.
// ---------------------------------------------------------------------------

/// Initial stack-buffer size used by [`read_edge_label`]. Most structural
/// labels (`child`, `parent`, `self`, `read`, `write`, `exec`,
/// `traverse`) fit comfortably under 32 bytes; the retry path handles
/// anything longer.
const READ_EDGE_LABEL_STACK: usize = 32;

/// Ceiling on the retry path's heap buffer, to bound pathological growth
/// if the kernel ever reports a huge label. 4 KiB is ~80 cache lines,
/// still one page — far more than any real label.
const READ_EDGE_LABEL_MAX: usize = 4096;

/// Read the full string label of the edge at `edge_index` in `src`'s
/// outgoing edge list, returning it as an owned [`String`].
///
/// [`list_edges`] reports edges as a cap-kind byte (`read`/`write`/
/// `exec`/`traverse`/`Unknown`) — structural labels like `child`,
/// `parent`, `self`, or anything else the graph carries show up as
/// [`Label::Unknown`]. Use this function to recover the actual label
/// string when you need it.
///
/// Indexing matches [`list_edges`] / [`list_edges_into`] (graph
/// insertion order).
///
/// Cap: requires a `traverse` edge from the caller to `src` — exactly
/// the same cap [`list_edges`] already required, so if the edge came
/// back from a successful `list_edges` call this will never return
/// [`Errno::Perm`] for cap reasons.
///
/// # Errors
///
/// - [`Errno::Perm`] — caller lacks a `traverse` edge to `src`.
/// - [`Errno::NotFound`] — `src` is missing, or `edge_index` is out of
///   range for that node.
/// - [`Errno::Invalid`] — a pathological label longer than
///   [`READ_EDGE_LABEL_MAX`] bytes (shouldn't happen in practice) or
///   a kernel-bounds-check failure (caller bug).
pub fn read_edge_label(src: NodeId, edge_index: usize) -> Result<String, Errno> {
    // Fast path: try a stack buffer first.
    let mut stack = [0u8; READ_EDGE_LABEL_STACK];
    match read_edge_label_into(src, edge_index, &mut stack) {
        Ok(n) => {
            // SAFETY: kernel stores UTF-8 strings; from_utf8_lossy
            // tolerates corruption without panicking.
            return Ok(String::from_utf8_lossy(&stack[..n]).into_owned());
        }
        Err(Errno::Invalid) => {
            // Could be "buf too small" (retry with bigger) or a true
            // EINVAL. Fall through to the heap retry; if the heap
            // attempt also returns Invalid, surface it to the caller.
        }
        Err(e) => return Err(e),
    }

    // Retry path: grow until the kernel accepts or we blow the cap.
    let mut cap = READ_EDGE_LABEL_STACK * 4;
    while cap <= READ_EDGE_LABEL_MAX {
        let mut heap = alloc::vec![0u8; cap];
        match read_edge_label_into(src, edge_index, &mut heap) {
            Ok(n) => {
                heap.truncate(n);
                return Ok(String::from_utf8_lossy(&heap).into_owned());
            }
            Err(Errno::Invalid) => {
                cap *= 2;
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    Err(Errno::Invalid)
}

/// Read the edge label into a caller-provided byte buffer. Returns the
/// number of bytes written.
///
/// Zero-allocation variant of [`read_edge_label`]. Useful when the
/// caller already knows an upper bound (or is running inside allocator
/// code). If `buf.len()` is shorter than the kernel-side label,
/// returns [`Errno::Invalid`] and the caller should retry with a
/// larger buffer; no bytes are written in that case.
pub fn read_edge_label_into(
    src: NodeId,
    edge_index: usize,
    buf: &mut [u8],
) -> Result<usize, Errno> {
    let r = unsafe {
        sys::sys_read_edge_label(src.0, edge_index, buf.as_mut_ptr(), buf.len())
    };
    if r < 0 {
        Err(Errno::from_raw(r))
    } else {
        Ok(r as usize)
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
/// The returned pointer is valid until either (a) the task exits — at
/// which point the kernel removes the `Memory` node, frees the
/// task→mem edge, and frees the page tables — or (b) the caller frees
/// the region explicitly via [`unmap_node`]. After [`unmap_node`] the
/// returned pointer is dangling and any access will trap; the caller
/// must not retain references into it.
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

// ---------------------------------------------------------------------------
// M35: SYS_DELEGATE_EDGE / SYS_REVOKE_EDGE — CDT-anchored cap flow.
// ---------------------------------------------------------------------------

/// Delegate one of the caller's outgoing edges onto another task. The
/// caller must hold a live outgoing edge labelled `label` pointing at
/// `target`, AND a live `grant` edge on `target`. On success a *derived*
/// edge is added to `to_task`'s outgoing list with the same `label` and
/// `target` — but its CDT parent points back to the caller's edge, so
/// revoking the caller's edge cascades and removes this one too.
///
/// Returns the new edge's raw vec_position on the recipient task node.
/// Callers that don't need this can discard it.
///
/// # Errors
///
/// - [`Errno::Perm`] — caller doesn't hold the source edge, OR doesn't
///   hold `grant` on `target`.
/// - [`Errno::NotFound`] — `to_task` or `target` missing from the graph.
/// - [`Errno::Invalid`] — `label` is empty, longer than 64 bytes, or
///   the kernel rejected the buffer.
pub fn delegate_edge(target: NodeId, to_task: NodeId, label: &str) -> Result<usize, Errno> {
    let r = unsafe {
        sys::sys_delegate_edge(target.0, to_task.0, label.as_ptr(), label.len())
    };
    if r < 0 {
        Err(Errno::from_raw(r))
    } else {
        Ok(r as usize)
    }
}

/// Revoke the caller's outgoing edge `(label, target)`. The edge and
/// every CDT descendant are tombstoned. For exec/read/write edges on
/// the active task, the corresponding PT slots are unmapped and the
/// TLB is flushed. Returns the count of tombstoned edges (>= 1).
///
/// # Errors
///
/// - [`Errno::NotFound`] — caller has no matching live outgoing edge.
/// - [`Errno::Invalid`] — `label` is empty, longer than 64 bytes, or
///   the kernel rejected the buffer (also: no active task, which
///   shouldn't happen from U-mode).
pub fn revoke_edge(target: NodeId, label: &str) -> Result<usize, Errno> {
    let r = unsafe {
        sys::sys_revoke_edge(target.0, label.as_ptr(), label.len())
    };
    if r < 0 {
        Err(Errno::from_raw(r))
    } else {
        Ok(r as usize)
    }
}

// ---------------------------------------------------------------------------
// Post-M35 (Proposal B): SYS_UNMAP_NODE — release a `SYS_MAP_NODE`
// allocation before task exit.
// ---------------------------------------------------------------------------

/// Release one of the caller's [`map_node`]-allocated memory regions.
/// The caller passes the `NodeId` returned (implicitly — via the
/// task's outgoing-edge list; see notes below) by an earlier
/// allocation. The kernel zaps the PT mappings, cascade-tombstones
/// the task's `write` edge to the node so any delegations also lose
/// access, removes the Memory node from the graph, and flushes the
/// TLB. Backing frames stay resident — per-frame reclaim is a future
/// milestone, matching the M33 footprint note.
///
/// # Finding the NodeId
///
/// [`map_node`] currently returns a raw user VA, not a [`NodeId`], so
/// callers that need to free a specific allocation will typically
/// enumerate the task's outgoing edges via [`list_edges`] and pick
/// the `Memory` target whose recorded VA matches the pointer they
/// want to free. Future revisions may return the `NodeId` directly
/// from `map_node`; for now this asymmetry is the M33 ABI.
///
/// # Errors
///
/// - [`Errno::NotFound`] — `node` is not in the caller's allocated
///   set (never minted by this task via [`map_node`]; already freed;
///   or the caller only has a delegated edge to it).
///
/// # Safety
///
/// After this call returns, every pointer the caller previously
/// obtained from [`map_node`] for `node` is dangling. The MMU
/// mapping is gone; loads/stores will trap. The caller is
/// responsible for ensuring no live `&` / `&mut` references into the
/// region exist before invoking `unmap_node`.
pub fn unmap_node(node: NodeId) -> Result<(), Errno> {
    let r = unsafe { sys::sys_unmap_node(node.0) };
    if r < 0 {
        Err(Errno::from_raw(r))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Host-side unit tests
// ---------------------------------------------------------------------------
//
// These tests exercise the pure-data parts of `helios-std` (Label
// encoding, Errno decoding, NodeId display, edge wire-format
// serialization). They do not call any syscall — all syscall paths
// are stubbed `unimplemented!()` on non-riscv64 hosts (see `sys.rs`
// module-level docs). Run via `make test-host` or
// `scripts/test-host.sh`.
//
// The point of these tests isn't coverage of the QEMU-tested logic —
// it's catching ABI-byte-level mistakes (wrong kernel kind constant,
// wrong endianness, wrong errno code) without a 30-second QEMU
// round-trip. They run in milliseconds and compound: every future
// session benefits.

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::string::ToString;

    // ------------------------------------------------------------------
    // Label round-trip and ABI constants.
    // ------------------------------------------------------------------

    /// Every byte must round-trip through `from_kind` → `as_kind`.
    /// This pins the encoding to a bijection in the input space and
    /// catches accidental "kind 5 also maps to Read" regressions.
    #[test]
    fn label_kind_round_trips_for_every_byte() {
        for kind in 0u8..=255 {
            let l = Label::from_kind(kind);
            assert_eq!(
                l.as_kind(),
                kind,
                "kind {} did not round-trip (got Label::{:?} → {})",
                kind,
                l,
                l.as_kind()
            );
        }
    }

    /// Pin the five named cap-kinds to their kernel ABI bytes. If
    /// these constants ever drift, every user binary's `list_edges`
    /// output silently misclassifies. Catching it here is cheaper
    /// than chasing a bad demo run.
    #[test]
    fn label_kind_constants_match_kernel_abi() {
        assert_eq!(Label::from_kind(1), Label::Read);
        assert_eq!(Label::from_kind(2), Label::Write);
        assert_eq!(Label::from_kind(3), Label::Exec);
        assert_eq!(Label::from_kind(4), Label::Traverse);
        assert_eq!(Label::from_kind(5), Label::Grant);

        assert_eq!(Label::Read.as_kind(), 1);
        assert_eq!(Label::Write.as_kind(), 2);
        assert_eq!(Label::Exec.as_kind(), 3);
        assert_eq!(Label::Traverse.as_kind(), 4);
        assert_eq!(Label::Grant.as_kind(), 5);
    }

    /// Kind 0 is documented as "unknown". Kernel structural edges
    /// (`child`, `parent`, `self`) come back as kind-byte 0 from
    /// `SYS_LIST_EDGES` and are surfaced via `SYS_READ_EDGE_LABEL`.
    #[test]
    fn label_zero_is_unknown() {
        assert_eq!(Label::from_kind(0), Label::Unknown(0));
    }

    /// Bytes 6..=255 are reserved/unknown — they must not collapse
    /// to the five named variants.
    #[test]
    fn label_unknown_byte_preserved() {
        for kind in 6u8..=255 {
            match Label::from_kind(kind) {
                Label::Unknown(b) => assert_eq!(b, kind),
                other => panic!(
                    "kind {} unexpectedly decoded to Label::{:?}",
                    kind, other
                ),
            }
        }
    }

    /// Canonical strings used by `ls-user` and the kernel's edge
    /// labels. Drifting these would break human-readable output.
    #[test]
    fn label_str_canonical() {
        assert_eq!(Label::Read.as_str(), "read");
        assert_eq!(Label::Write.as_str(), "write");
        assert_eq!(Label::Exec.as_str(), "exec");
        assert_eq!(Label::Traverse.as_str(), "traverse");
        assert_eq!(Label::Grant.as_str(), "grant");
        assert_eq!(Label::Unknown(0).as_str(), "?");
        assert_eq!(Label::Unknown(99).as_str(), "?");
    }

    /// Display impl matches `as_str` for the named kinds.
    #[test]
    fn label_display_matches_as_str() {
        for l in [
            Label::Read,
            Label::Write,
            Label::Exec,
            Label::Traverse,
            Label::Grant,
        ] {
            assert_eq!(format!("{}", l), l.as_str());
        }
        assert_eq!(format!("{}", Label::Unknown(7)), "?");
    }

    // ------------------------------------------------------------------
    // Errno decoding.
    // ------------------------------------------------------------------

    /// Kernel errno bytes -> typed Errno mapping. These are pinned by
    /// the kernel's syscall convention; if the kernel renumbers, this
    /// catches it.
    #[test]
    fn errno_named_constants() {
        assert_eq!(Errno::from_raw(-1), Errno::Perm);
        assert_eq!(Errno::from_raw(-2), Errno::NotFound);
        assert_eq!(Errno::from_raw(-3), Errno::Invalid);
        assert_eq!(Errno::from_raw(-4), Errno::NoMem);
    }

    /// Other negative values fall through to `Other(n)` rather than
    /// being silently mapped to one of the named variants.
    #[test]
    fn errno_unknown_falls_through_to_other() {
        for &v in &[-5isize, -42, -99, -1000] {
            match Errno::from_raw(v) {
                Errno::Other(got) => assert_eq!(got, v),
                other => panic!("errno {} → unexpected {:?}", v, other),
            }
        }
    }

    /// Display impl produces the human-readable codes user programs
    /// print on error.
    #[test]
    fn errno_display() {
        assert_eq!(Errno::Perm.to_string(), "EPERM");
        assert_eq!(Errno::NotFound.to_string(), "ENOENT");
        assert_eq!(Errno::Invalid.to_string(), "EINVAL");
        assert_eq!(Errno::NoMem.to_string(), "ENOMEM");
        assert_eq!(Errno::Other(-77).to_string(), "E(-77)");
    }

    // ------------------------------------------------------------------
    // NodeId.
    // ------------------------------------------------------------------

    /// `Display` is `#N`. `ls-user` and the navigator both rely on
    /// this format being stable.
    #[test]
    fn node_id_display() {
        assert_eq!(NodeId(0).to_string(), "#0");
        assert_eq!(NodeId(1).to_string(), "#1");
        assert_eq!(NodeId(42).to_string(), "#42");
        assert_eq!(NodeId(u64::MAX).to_string(), format!("#{}", u64::MAX));
    }

    /// `Debug` is `NodeId(N)`.
    #[test]
    fn node_id_debug() {
        assert_eq!(format!("{:?}", NodeId(7)), "NodeId(7)");
    }

    /// Equality / ordering / hash: the derive-based impls treat
    /// NodeId as transparent over its u64. Pin a couple of cases so
    /// a future #[repr(...)] change is loud.
    #[test]
    fn node_id_ordering() {
        assert!(NodeId(1) < NodeId(2));
        assert!(NodeId(2) > NodeId(1));
        assert_eq!(NodeId(5), NodeId(5));
        assert_ne!(NodeId(5), NodeId(6));
    }

    // ------------------------------------------------------------------
    // EdgeInfo defaults.
    // ------------------------------------------------------------------

    #[test]
    fn edge_info_default_is_zero_id_unknown_zero() {
        let e = EdgeInfo::default();
        assert_eq!(e.target, NodeId(0));
        assert_eq!(e.label, Label::Unknown(0));
    }

    // ------------------------------------------------------------------
    // Edge wire-format decode.
    // ------------------------------------------------------------------
    //
    // Kernel ABI: 16 bytes per edge — first 8 bytes are the target
    // NodeId in little-endian, byte 8 is the label kind, bytes 9..16
    // are reserved/padding. These tests pin all three: endianness,
    // padding-tolerance, and label-kind decoding.

    #[test]
    fn decode_edge_entry_zeroed() {
        let entry = [0u8; EDGE_ENTRY_SIZE];
        let e = decode_edge_entry(&entry);
        assert_eq!(e.target, NodeId(0));
        assert_eq!(e.label, Label::Unknown(0));
    }

    #[test]
    fn decode_edge_entry_low_byte_only() {
        let mut entry = [0u8; EDGE_ENTRY_SIZE];
        entry[0] = 0x42; // u64 LE: 0x0000_0000_0000_0042
        entry[8] = 1; // Label::Read
        let e = decode_edge_entry(&entry);
        assert_eq!(e.target, NodeId(0x42));
        assert_eq!(e.label, Label::Read);
    }

    #[test]
    fn decode_edge_entry_full_u64_little_endian() {
        // 0x0123_4567_89AB_CDEF in little-endian byte order.
        let entry: [u8; EDGE_ENTRY_SIZE] = [
            0xEF, 0xCD, 0xAB, 0x89, 0x67, 0x45, 0x23, 0x01, // target
            4, // Label::Traverse
            0, 0, 0, 0, 0, 0, 0, // padding (kernel currently zeroes)
        ];
        let e = decode_edge_entry(&entry);
        assert_eq!(e.target, NodeId(0x0123_4567_89AB_CDEF));
        assert_eq!(e.label, Label::Traverse);
    }

    /// Decoder must ignore bytes 9..16 — they are reserved padding.
    /// If a future kernel ever uses them, an explicit ABI bump is
    /// required, not silent reinterpretation.
    #[test]
    fn decode_edge_entry_padding_ignored() {
        let mut entry = [0u8; EDGE_ENTRY_SIZE];
        entry[0..8].copy_from_slice(&100u64.to_le_bytes());
        entry[8] = 2; // Label::Write
        // Set every padding byte to 0xFF — must not affect the
        // decoded result.
        for b in &mut entry[9..16] {
            *b = 0xFF;
        }
        let e = decode_edge_entry(&entry);
        assert_eq!(e.target, NodeId(100));
        assert_eq!(e.label, Label::Write);
    }

    /// Highest-bit set in target id round-trips intact (catches any
    /// accidental signed coercion in the decode path).
    #[test]
    fn decode_edge_entry_high_bit_target() {
        let mut entry = [0u8; EDGE_ENTRY_SIZE];
        let id = u64::MAX;
        entry[0..8].copy_from_slice(&id.to_le_bytes());
        entry[8] = 3; // Label::Exec
        let e = decode_edge_entry(&entry);
        assert_eq!(e.target, NodeId(u64::MAX));
        assert_eq!(e.label, Label::Exec);
    }
}
