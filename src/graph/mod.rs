/// In-memory graph store for Helios.
/// "Everything is a memory" — nodes with typed content and labeled edges.

pub mod compute;
pub mod init;
pub mod live;
pub mod navigator;
pub mod persist;
pub mod query;
pub mod render;
pub mod user;
pub mod window;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeType {
    Text,
    Binary,
    Config,
    System,
    Directory,
    Computed,
    Channel,
    /// Anonymous writable memory granted to a user task via
    /// `SYS_MAP_NODE` (M33). Backed by frames mapped into the task's
    /// data-VA window; the node itself is a graph citizen like any
    /// other, so delegation / introspection stay thesis-aligned.
    Memory,
}

impl NodeType {
    pub fn from_str(s: &str) -> Option<NodeType> {
        match s {
            "text" => Some(NodeType::Text),
            "binary" => Some(NodeType::Binary),
            "config" => Some(NodeType::Config),
            "system" => Some(NodeType::System),
            "dir" => Some(NodeType::Directory),
            "computed" | "comp" => Some(NodeType::Computed),
            "channel" => Some(NodeType::Channel),
            "memory" | "mem" => Some(NodeType::Memory),
            _ => None,
        }
    }
}

impl fmt::Display for NodeType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeType::Text => write!(f, "text"),
            NodeType::Binary => write!(f, "binary"),
            NodeType::Config => write!(f, "config"),
            NodeType::System => write!(f, "system"),
            NodeType::Directory => write!(f, "dir"),
            NodeType::Computed => write!(f, "computed"),
            NodeType::Channel => write!(f, "channel"),
            NodeType::Memory => write!(f, "memory"),
        }
    }
}

/// Stable identity for an edge across its lifetime.
///
/// `(src_node_id, vec_position)`. Once an edge is added at a given vec
/// position, that position is never reused: tombstoning flips `live=false`
/// in place, and removed/cascaded edges leave their slot vacant. So this
/// pair uniquely names an edge for as long as the graph is alive.
///
/// CDT (M35) walks `derived_from` chains over these IDs to compute
/// revocation cascades.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EdgeId(pub u64, pub u32);

impl EdgeId {
    pub fn src(self) -> u64 { self.0 }
    pub fn idx(self) -> u32 { self.1 }
}

#[derive(Debug, Clone)]
pub struct Edge {
    pub label: String,
    pub target: u64,
    /// `false` once the edge has been tombstoned (revoke / remove_node /
    /// task-exit). Tombstoned edges are *retained in place* so that
    /// outstanding `EdgeId`s remain valid; iteration helpers skip them.
    pub live: bool,
    /// Capability-derivation lineage: if this edge was created by
    /// `SYS_DELEGATE_EDGE`, this points at the parent. Root edges
    /// (kernel-declared at boot or via `add_edge`) have `None`.
    pub derived_from: Option<EdgeId>,
}

impl Edge {
    /// A fresh root edge — live, no parent.
    pub fn root(label: &str, target: u64) -> Self {
        Edge {
            label: String::from(label),
            target,
            live: true,
            derived_from: None,
        }
    }

    /// A delegated child edge, tracking its parent in the CDT.
    pub fn derived(label: &str, target: u64, parent: EdgeId) -> Self {
        Edge {
            label: String::from(label),
            target,
            live: true,
            derived_from: Some(parent),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Node {
    pub id: u64,
    pub type_tag: NodeType,
    pub name: String,
    pub content: Vec<u8>,
    pub edges: Vec<Edge>,
}

impl Node {
    /// Iterate over live (non-tombstoned) edges along with their stable
    /// `EdgeId`. Tombstoned slots are skipped but still consume an index.
    pub fn live_edges(&self) -> impl Iterator<Item = (EdgeId, &Edge)> {
        let src = self.id;
        self.edges
            .iter()
            .enumerate()
            .filter(|(_, e)| e.live)
            .map(move |(i, e)| (EdgeId(src, i as u32), e))
    }

    /// Just the live edges, without ids — most callsites only need this.
    pub fn iter_live(&self) -> impl Iterator<Item = &Edge> {
        self.edges.iter().filter(|e| e.live)
    }

    /// Count of live edges (skipping tombstones).
    pub fn live_edge_count(&self) -> usize {
        self.edges.iter().filter(|e| e.live).count()
    }
}

impl Node {
    /// Return the display content for this node. For computed nodes, evaluates
    /// the formula; for others, returns the content as a string (or a placeholder).
    pub fn display_content(&self, graph: &Graph) -> alloc::string::String {
        if self.type_tag == NodeType::Computed {
            let formula = core::str::from_utf8(&self.content).unwrap_or("");
            compute::evaluate(formula, graph)
        } else if self.content.is_empty() {
            alloc::string::String::from("(empty)")
        } else {
            match core::str::from_utf8(&self.content) {
                Ok(s) => alloc::string::String::from(s),
                Err(_) => alloc::format!("({} bytes, binary)", self.content.len()),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Graph
// ---------------------------------------------------------------------------

pub struct Graph {
    pub nodes: BTreeMap<u64, Node>,
    pub next_id: u64,
}

impl Graph {
    pub fn new() -> Self {
        Graph {
            nodes: BTreeMap::new(),
            next_id: 1,
        }
    }

    /// Create a node and return its ID.
    pub fn create_node(&mut self, type_tag: NodeType, name: &str) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let node = Node {
            id,
            type_tag,
            name: String::from(name),
            content: Vec::new(),
            edges: Vec::new(),
        };
        self.nodes.insert(id, node);
        id
    }

    pub fn get_node(&self, id: u64) -> Option<&Node> {
        self.nodes.get(&id)
    }

    pub fn get_node_mut(&mut self, id: u64) -> Option<&mut Node> {
        self.nodes.get_mut(&id)
    }

    /// Append a root edge `from --label--> to`. Returns the new edge's
    /// stable `EdgeId`, or `None` if either endpoint is missing.
    ///
    /// Tombstoned slots are *not* reused — `vec_position` keeps growing.
    /// This is what gives `EdgeId` its stability.
    pub fn add_edge_id(&mut self, from: u64, label: &str, to: u64) -> Option<EdgeId> {
        if !self.nodes.contains_key(&to) {
            return None;
        }
        let node = self.nodes.get_mut(&from)?;
        let idx = node.edges.len() as u32;
        node.edges.push(Edge::root(label, to));
        Some(EdgeId(from, idx))
    }

    /// Append an edge that is derived from a parent (CDT delegation).
    /// The caller is expected to have already validated the `grant`
    /// authority — this function only updates graph state.
    pub fn add_derived_edge(
        &mut self,
        from: u64,
        label: &str,
        to: u64,
        parent: EdgeId,
    ) -> Option<EdgeId> {
        if !self.nodes.contains_key(&to) {
            return None;
        }
        let node = self.nodes.get_mut(&from)?;
        let idx = node.edges.len() as u32;
        node.edges.push(Edge::derived(label, to, parent));
        Some(EdgeId(from, idx))
    }

    /// Convenience wrapper preserving the old `bool` API. Most existing
    /// kernel sites just want "did this work?" — they get back a bool;
    /// CDT-aware sites use `add_edge_id` for the EdgeId.
    pub fn add_edge(&mut self, from: u64, label: &str, to: u64) -> bool {
        self.add_edge_id(from, label, to).is_some()
    }

    /// Look up a single live edge by its stable id.
    pub fn get_edge(&self, id: EdgeId) -> Option<&Edge> {
        let node = self.nodes.get(&id.0)?;
        let edge = node.edges.get(id.1 as usize)?;
        if edge.live { Some(edge) } else { None }
    }

    /// Tombstone a single edge by id. Returns `true` if it was live and
    /// is now tombstoned. Does not cascade — see `cascade_tombstone`.
    pub fn tombstone_edge(&mut self, id: EdgeId) -> bool {
        let Some(node) = self.nodes.get_mut(&id.0) else { return false; };
        let Some(edge) = node.edges.get_mut(id.1 as usize) else { return false; };
        if !edge.live { return false; }
        edge.live = false;
        true
    }

    /// Find every edge whose CDT lineage traces back through `root`,
    /// inclusive of `root` itself if live. BFS over `derived_from`.
    ///
    /// Cost: O(total live edges in graph) — we scan the whole graph for
    /// children at each level. Helios graphs are small; the state note
    /// (2026-05-07 §"cost paid in space") says profile before optimising.
    pub fn find_descendants(&self, root: EdgeId) -> Vec<EdgeId> {
        let mut out = Vec::new();
        let mut frontier: Vec<EdgeId> = Vec::new();
        if let Some(e) = self.get_edge(root) {
            let _ = e; // include root itself if live
            out.push(root);
            frontier.push(root);
        }
        while let Some(parent) = frontier.pop() {
            // Scan all live edges in the graph; collect those whose
            // derived_from points at `parent`.
            for node in self.nodes.values() {
                for (i, edge) in node.edges.iter().enumerate() {
                    if !edge.live { continue; }
                    if edge.derived_from == Some(parent) {
                        let child = EdgeId(node.id, i as u32);
                        if !out.contains(&child) {
                            out.push(child);
                            frontier.push(child);
                        }
                    }
                }
            }
        }
        out
    }

    /// Tombstone an edge and every edge derived from it, transitively.
    /// Returns the list of `EdgeId`s that were tombstoned (in BFS order
    /// rooted at `root`). Callers (kernel) need this list to invalidate
    /// page tables / cap caches downstream.
    ///
    /// Idempotent: cascading on an already-tombstoned root is a no-op
    /// returning an empty Vec.
    pub fn cascade_tombstone(&mut self, root: EdgeId) -> Vec<EdgeId> {
        let descendants = self.find_descendants(root);
        for &id in &descendants {
            // Already-checked-live in find_descendants, but we re-check
            // because mutation between calls is not guarded here.
            if let Some(node) = self.nodes.get_mut(&id.0) {
                if let Some(edge) = node.edges.get_mut(id.1 as usize) {
                    edge.live = false;
                }
            }
        }
        descendants
    }

    /// Remove a node from the graph. Tombstones every incoming edge that
    /// pointed at the removed node — preserves `EdgeId` stability for
    /// surviving edges of unaffected nodes.
    ///
    /// Note: this does **not** cascade-tombstone via CDT. Removing a
    /// node yanks the underlying citizen; whether any caps that targeted
    /// it should also have their derived caps revoked is policy left to
    /// the caller. (Practically: kernel callers run task-exit cleanup,
    /// which cascades a task's *outgoing* edges separately before
    /// `remove_node` is called.)
    pub fn remove_node(&mut self, id: u64) -> bool {
        if self.nodes.remove(&id).is_none() {
            return false;
        }
        // Tombstone (don't shift) edges pointing to the removed node.
        for node in self.nodes.values_mut() {
            for edge in node.edges.iter_mut() {
                if edge.target == id {
                    edge.live = false;
                }
            }
        }
        true
    }

    pub fn find_by_name(&self, substring: &str) -> Vec<&Node> {
        self.nodes
            .values()
            .filter(|n| n.name.contains(substring))
            .collect()
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Count of live edges across the whole graph. Tombstones excluded.
    pub fn edge_count(&self) -> usize {
        self.nodes.values().map(|n| n.live_edge_count()).sum()
    }

    /// Count of slots — live edges plus tombstones. Useful for
    /// diagnostics / tests; not for ABI surfacing.
    #[allow(dead_code)]
    pub fn edge_slot_count(&self) -> usize {
        self.nodes.values().map(|n| n.edges.len()).sum()
    }
}

// ---------------------------------------------------------------------------
// Global instance
// ---------------------------------------------------------------------------

static mut GRAPH: Option<Graph> = None;

/// Get a shared reference to the global graph. Panics if not initialized.
#[allow(static_mut_refs)]
pub fn get() -> &'static Graph {
    unsafe { GRAPH.as_ref().expect("graph not initialized") }
}

/// Get a mutable reference to the global graph. Panics if not initialized.
#[allow(static_mut_refs)]
pub fn get_mut() -> &'static mut Graph {
    unsafe { GRAPH.as_mut().expect("graph not initialized") }
}

/// Replace the global graph with a new one (used by load).
#[allow(static_mut_refs)]
pub fn replace(graph: Graph) {
    unsafe {
        GRAPH = Some(graph);
    }
}

/// Initialize the global graph and bootstrap initial nodes.
#[allow(static_mut_refs)]
pub fn init() {
    unsafe {
        GRAPH = Some(Graph::new());
    }
    init::bootstrap();
    let g = get();
    crate::println!(
        "[graph] Initialized: {} nodes, {} edges",
        g.node_count(),
        g.edge_count()
    );
}
