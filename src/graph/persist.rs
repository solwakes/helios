/// Graph serialization/deserialization for disk persistence.
///
/// Binary format:
///   Magic: "HGRF" (4 bytes)
///   Version: u32 (1)
///   Node count: u32
///   For each node:
///     id: u64
///     type_tag: u8
///     name_len: u16
///     name: [u8; name_len]
///     content_len: u32
///     content: [u8; content_len]
///     edge_count: u16
///     For each edge:
///       label_len: u16
///       label: [u8; label_len]
///       target: u64
///   next_id: u64

use alloc::string::String;
use alloc::vec::Vec;
use super::{Graph, Node, Edge, NodeType};

const MAGIC: &[u8; 4] = b"HGRF";
const VERSION: u32 = 1;

fn push_u16(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn push_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn push_u64(buf: &mut Vec<u8>, v: u64) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn read_u16(data: &[u8], off: &mut usize) -> Option<u16> {
    if *off + 2 > data.len() { return None; }
    let v = u16::from_le_bytes([data[*off], data[*off + 1]]);
    *off += 2;
    Some(v)
}

fn read_u32(data: &[u8], off: &mut usize) -> Option<u32> {
    if *off + 4 > data.len() { return None; }
    let v = u32::from_le_bytes([data[*off], data[*off+1], data[*off+2], data[*off+3]]);
    *off += 4;
    Some(v)
}

fn read_u64(data: &[u8], off: &mut usize) -> Option<u64> {
    if *off + 8 > data.len() { return None; }
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&data[*off..*off + 8]);
    let v = u64::from_le_bytes(bytes);
    *off += 8;
    Some(v)
}

fn read_bytes<'a>(data: &'a [u8], off: &mut usize, len: usize) -> Option<&'a [u8]> {
    if *off + len > data.len() { return None; }
    let slice = &data[*off..*off + len];
    *off += len;
    Some(slice)
}

fn type_to_u8(t: NodeType) -> u8 {
    match t {
        NodeType::Text => 0,
        NodeType::Binary => 1,
        NodeType::Config => 2,
        NodeType::System => 3,
        NodeType::Directory => 4,
        NodeType::Computed => 5,
    }
}

fn u8_to_type(v: u8) -> Option<NodeType> {
    match v {
        0 => Some(NodeType::Text),
        1 => Some(NodeType::Binary),
        2 => Some(NodeType::Config),
        3 => Some(NodeType::System),
        4 => Some(NodeType::Directory),
        5 => Some(NodeType::Computed),
        _ => None,
    }
}

/// Serialize a graph to bytes.
pub fn serialize(graph: &Graph) -> Vec<u8> {
    let mut buf = Vec::new();

    // Magic + version
    buf.extend_from_slice(MAGIC);
    push_u32(&mut buf, VERSION);

    // Node count
    push_u32(&mut buf, graph.nodes.len() as u32);

    // Each node
    for node in graph.nodes.values() {
        push_u64(&mut buf, node.id);
        buf.push(type_to_u8(node.type_tag));

        let name_bytes = node.name.as_bytes();
        push_u16(&mut buf, name_bytes.len() as u16);
        buf.extend_from_slice(name_bytes);

        push_u32(&mut buf, node.content.len() as u32);
        buf.extend_from_slice(&node.content);

        push_u16(&mut buf, node.edges.len() as u16);
        for edge in &node.edges {
            let label_bytes = edge.label.as_bytes();
            push_u16(&mut buf, label_bytes.len() as u16);
            buf.extend_from_slice(label_bytes);
            push_u64(&mut buf, edge.target);
        }
    }

    // next_id
    push_u64(&mut buf, graph.next_id);

    buf
}

/// Deserialize bytes to a graph. Returns None on invalid data.
pub fn deserialize(data: &[u8]) -> Option<Graph> {
    let mut off = 0usize;

    // Magic
    let magic = read_bytes(data, &mut off, 4)?;
    if magic != MAGIC {
        return None;
    }

    // Version
    let version = read_u32(data, &mut off)?;
    if version != VERSION {
        return None;
    }

    let node_count = read_u32(data, &mut off)? as usize;

    let mut graph = Graph::new();

    for _ in 0..node_count {
        let id = read_u64(data, &mut off)?;
        let type_byte = if off < data.len() { let b = data[off]; off += 1; b } else { return None; };
        let type_tag = u8_to_type(type_byte)?;

        let name_len = read_u16(data, &mut off)? as usize;
        let name_bytes = read_bytes(data, &mut off, name_len)?;
        let name = String::from(core::str::from_utf8(name_bytes).ok()?);

        let content_len = read_u32(data, &mut off)? as usize;
        let content_bytes = read_bytes(data, &mut off, content_len)?;
        let content = Vec::from(content_bytes);

        let edge_count = read_u16(data, &mut off)? as usize;
        let mut edges = Vec::with_capacity(edge_count);
        for _ in 0..edge_count {
            let label_len = read_u16(data, &mut off)? as usize;
            let label_bytes = read_bytes(data, &mut off, label_len)?;
            let label = String::from(core::str::from_utf8(label_bytes).ok()?);
            let target = read_u64(data, &mut off)?;
            edges.push(Edge { label, target });
        }

        let node = Node {
            id,
            type_tag,
            name,
            content,
            edges,
        };
        graph.nodes.insert(id, node);
    }

    let next_id = read_u64(data, &mut off)?;
    graph.next_id = next_id;

    Some(graph)
}
