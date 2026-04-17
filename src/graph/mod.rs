/// In-memory graph store for Helios.
/// "Everything is a memory" — nodes with typed content and labeled edges.

pub mod compute;
pub mod init;
pub mod live;
pub mod navigator;
pub mod persist;
pub mod query;
pub mod render;
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
        }
    }
}

#[derive(Debug, Clone)]
pub struct Edge {
    pub label: String,
    pub target: u64,
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

    pub fn add_edge(&mut self, from: u64, label: &str, to: u64) -> bool {
        if !self.nodes.contains_key(&to) {
            return false;
        }
        if let Some(node) = self.nodes.get_mut(&from) {
            node.edges.push(Edge {
                label: String::from(label),
                target: to,
            });
            true
        } else {
            false
        }
    }

    pub fn remove_node(&mut self, id: u64) -> bool {
        if self.nodes.remove(&id).is_none() {
            return false;
        }
        // Remove edges pointing to this node in all remaining nodes
        for node in self.nodes.values_mut() {
            node.edges.retain(|e| e.target != id);
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

    pub fn edge_count(&self) -> usize {
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
