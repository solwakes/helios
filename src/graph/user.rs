/// The `/user` subgraph — a safe home for externally-created nodes.
///
/// When the HTTP server receives a `POST /nodes`, the new node is attached
/// under the `/user` directory node (not directly under `/root`) so external
/// clients can't pollute the system graph. A side-table keyed by node ID
/// records the remote IP, tick count, and uptime at creation, so we can tell
/// "user" nodes from system nodes at a glance via the `users` shell command.
///
/// This module is intentionally tiny: it doesn't own any graph data, it just
/// indexes which node IDs were created externally and where they came from.
/// When a user node is deleted (via DELETE /nodes/{id} or the `clear users`
/// shell command), we both remove the node from the graph and forget it here.
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use super::{get_mut, NodeType};

/// Graph ID of the `/user` directory node. Populated by `init()`.
static mut USER_DIR_NODE_ID: u64 = 0;

/// Metadata tracked for every externally-created node.
#[derive(Clone, Copy)]
pub struct UserInfo {
    /// Remote IPv4 address of the client that POSTed the node.
    pub source_ip: [u8; 4],
    /// Timer tick count at creation (via `trap::tick_count()`).
    pub created_tick: usize,
    /// Kernel uptime in seconds at creation.
    pub created_uptime_s: u64,
}

/// The side-table — lazily initialized by `init()`.
static mut USER_INFO: Option<BTreeMap<u64, UserInfo>> = None;

/// Initialize the `/user` subgraph. Creates a Directory node named "user"
/// under root (ID 1) and wires up the side-table. Call once, after
/// `graph::init()` and before any remote writes can occur.
#[allow(static_mut_refs)]
pub fn init() {
    let g = get_mut();
    let id = g.create_node(NodeType::Directory, "user");
    // root is well-known as ID 1.
    g.add_edge(1, "child", id);
    unsafe {
        USER_DIR_NODE_ID = id;
        USER_INFO = Some(BTreeMap::new());
    }
    crate::println!("[user] /user subgraph ready (node #{})", id);
}

/// Returns the graph ID of the `/user` directory, or 0 if not initialized.
#[allow(static_mut_refs)]
pub fn user_dir_id() -> u64 {
    unsafe { USER_DIR_NODE_ID }
}

/// Record a newly-created external node along with its origin metadata.
#[allow(static_mut_refs)]
pub fn register(node_id: u64, source_ip: [u8; 4]) {
    let tick = crate::trap::tick_count();
    // QEMU virt timer: 10 MHz → seconds = time / 10_000_000.
    let time = crate::arch::riscv64::read_time() as u64;
    let uptime_s = time / 10_000_000;
    unsafe {
        if let Some(m) = USER_INFO.as_mut() {
            m.insert(
                node_id,
                UserInfo {
                    source_ip,
                    created_tick: tick,
                    created_uptime_s: uptime_s,
                },
            );
        }
    }
}

/// Is this node ID in our externally-created side-table?
#[allow(static_mut_refs)]
pub fn is_user_node(id: u64) -> bool {
    unsafe {
        USER_INFO
            .as_ref()
            .map(|m| m.contains_key(&id))
            .unwrap_or(false)
    }
}

/// Drop the side-table entry for this ID. Call after deleting the node from
/// the graph — this doesn't touch the graph itself.
#[allow(static_mut_refs)]
pub fn forget(id: u64) {
    unsafe {
        if let Some(m) = USER_INFO.as_mut() {
            m.remove(&id);
        }
    }
}

/// Snapshot every user-node entry (id → metadata), sorted by ID.
#[allow(static_mut_refs)]
pub fn all() -> Vec<(u64, UserInfo)> {
    unsafe {
        match USER_INFO.as_ref() {
            Some(m) => m.iter().map(|(k, v)| (*k, *v)).collect(),
            None => Vec::new(),
        }
    }
}

/// How many externally-created nodes are tracked.
#[allow(static_mut_refs)]
pub fn count() -> usize {
    unsafe { USER_INFO.as_ref().map(|m| m.len()).unwrap_or(0) }
}
