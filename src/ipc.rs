/// Graph-based IPC (Inter-Process Communication) for Helios.
///
/// Tasks communicate by reading and writing to shared graph nodes of type
/// `Channel`. Channels live under root -> ipc -> [channels] in the graph
/// tree, making IPC visible, queryable, and navigable.

use alloc::string::String;
use alloc::vec::Vec;
use crate::graph::{self, NodeType};

/// Well-known graph node ID for the "ipc" directory.
static mut IPC_DIR_NODE_ID: u64 = 0;

/// Initialize the IPC subsystem. Creates the "ipc" directory node under root.
/// Must be called after graph::init().
pub fn init() {
    let g = graph::get_mut();
    let ipc_id = g.create_node(NodeType::Directory, "ipc");
    g.add_edge(1, "child", ipc_id);
    unsafe { IPC_DIR_NODE_ID = ipc_id; }
    crate::println!("[ipc] IPC subsystem initialized (channels under node #{})", ipc_id);
}

/// Create a new IPC channel with the given name. Returns the channel's node ID.
pub fn create_channel(name: &str) -> u64 {
    let ipc_dir = unsafe { IPC_DIR_NODE_ID };
    let g = graph::get_mut();
    let ch_id = g.create_node(NodeType::Channel, name);
    g.add_edge(ipc_dir, "child", ch_id);
    ch_id
}

/// Send a message to a channel (append to the message queue).
/// Messages are stored as newline-separated entries in the node content.
pub fn send(channel_id: u64, msg: &str) {
    let g = graph::get_mut();
    if let Some(node) = g.get_node_mut(channel_id) {
        if node.type_tag != NodeType::Channel {
            return;
        }
        // Append message: if content is non-empty, add a newline separator first
        if !node.content.is_empty() {
            node.content.push(b'\n');
        }
        node.content.extend_from_slice(msg.as_bytes());
    }
}

/// Receive (pop) the oldest message from a channel.
/// Removes the first line from the node's content and returns it.
pub fn recv(channel_id: u64) -> Option<String> {
    let g = graph::get_mut();
    let node = g.get_node_mut(channel_id)?;
    if node.type_tag != NodeType::Channel {
        return None;
    }
    if node.content.is_empty() {
        return None;
    }
    let content = core::str::from_utf8(&node.content).ok()?;
    if let Some(pos) = content.find('\n') {
        let first = String::from(&content[..pos]);
        let rest = Vec::from(content[pos + 1..].as_bytes());
        node.content = rest;
        Some(first)
    } else {
        // Only one message — take everything
        let msg = String::from(content);
        node.content.clear();
        Some(msg)
    }
}

/// Peek at the oldest message without consuming it.
pub fn peek(channel_id: u64) -> Option<String> {
    let g = graph::get();
    let node = g.get_node(channel_id)?;
    if node.type_tag != NodeType::Channel {
        return None;
    }
    if node.content.is_empty() {
        return None;
    }
    let content = core::str::from_utf8(&node.content).ok()?;
    if let Some(pos) = content.find('\n') {
        Some(String::from(&content[..pos]))
    } else {
        Some(String::from(content))
    }
}

/// Broadcast: overwrite the channel content (last-writer-wins, for pub/sub).
pub fn broadcast(channel_id: u64, msg: &str) {
    let g = graph::get_mut();
    if let Some(node) = g.get_node_mut(channel_id) {
        if node.type_tag != NodeType::Channel {
            return;
        }
        node.content = Vec::from(msg.as_bytes());
    }
}

/// Read the full current content of a channel without consuming.
pub fn read(channel_id: u64) -> Option<String> {
    let g = graph::get();
    let node = g.get_node(channel_id)?;
    if node.type_tag != NodeType::Channel {
        return None;
    }
    if node.content.is_empty() {
        return None;
    }
    core::str::from_utf8(&node.content).ok().map(String::from)
}

/// Return the IPC directory node ID.
pub fn ipc_dir_id() -> u64 {
    unsafe { IPC_DIR_NODE_ID }
}

/// List all IPC channels. Returns (node_id, name, message_count, content_preview).
pub fn list_channels() -> Vec<(u64, String, usize, String)> {
    let g = graph::get();
    let ipc_dir = unsafe { IPC_DIR_NODE_ID };
    let mut channels = Vec::new();

    if let Some(dir_node) = g.get_node(ipc_dir) {
        for edge in &dir_node.edges {
            if edge.label == "child" {
                if let Some(node) = g.get_node(edge.target) {
                    if node.type_tag == NodeType::Channel {
                        let content_str = core::str::from_utf8(&node.content).unwrap_or("");
                        let msg_count = if content_str.is_empty() {
                            0
                        } else {
                            content_str.lines().count()
                        };
                        let preview = if content_str.len() > 60 {
                            let mut s = String::from(&content_str[..57]);
                            s.push_str("...");
                            s
                        } else {
                            String::from(content_str)
                        };
                        channels.push((node.id, node.name.clone(), msg_count, preview));
                    }
                }
            }
        }
    }

    channels
}
