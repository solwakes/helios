/// Graph Query Language (GQL) for Helios.
///
/// A mini DSL for querying, filtering, and traversing the graph.

use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;
use super::{Graph, Node, NodeType};

// ---------------------------------------------------------------------------
// Query types
// ---------------------------------------------------------------------------

enum Query<'a> {
    /// Filter nodes by criteria
    Filter(Filter<'a>),
    /// Count matching nodes (or all)
    Count(Option<Filter<'a>>),
    /// Direct children of a node, optionally filtered by edge label
    Children(u64, Option<&'a str>),
    /// Find parents (nodes with edges pointing to target)
    Parent(u64),
    /// All descendants via BFS
    Descendants(u64),
    /// Shortest path between two nodes
    Path(u64, u64),
}

enum Filter<'a> {
    TypeEq(&'a str),
    NameContains(&'a str),
    IdEq(u64),
    ContentContains(&'a str),
    EdgesGt(usize),
    EdgesEq(usize),
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Parse a query string into a Query enum.
fn parse_query(input: &str) -> Option<Query<'_>> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }

    // Split into tokens (up to 4)
    let mut tokens: Vec<&str> = Vec::new();
    let mut rest = input;
    for _ in 0..4 {
        let r = rest.trim_start();
        if r.is_empty() {
            break;
        }
        match r.find(' ') {
            Some(pos) => {
                tokens.push(&r[..pos]);
                rest = &r[pos + 1..];
            }
            None => {
                tokens.push(r);
                rest = "";
            }
        }
    }

    if tokens.is_empty() {
        return None;
    }

    let first = tokens[0];

    // Traversal commands
    match first {
        "children" => {
            let id = parse_id(tokens.get(1)?)?;
            let label = tokens.get(2).copied();
            return Some(Query::Children(id, label));
        }
        "parent" => {
            let id = parse_id(tokens.get(1)?)?;
            return Some(Query::Parent(id));
        }
        "descendants" => {
            let id = parse_id(tokens.get(1)?)?;
            return Some(Query::Descendants(id));
        }
        "path" => {
            let from = parse_id(tokens.get(1)?)?;
            let to = parse_id(tokens.get(2)?)?;
            return Some(Query::Path(from, to));
        }
        "count" => {
            if tokens.len() > 1 {
                let filter = parse_filter(tokens[1])?;
                return Some(Query::Count(Some(filter)));
            } else {
                return Some(Query::Count(None));
            }
        }
        _ => {}
    }

    // Try to parse as a filter (possibly with pipe)
    let filter = parse_filter(first)?;
    Some(Query::Filter(filter))
}

/// Parse a filter expression like "type=system", "name~mem", "edges>2"
fn parse_filter(expr: &str) -> Option<Filter<'_>> {
    // Try type=
    if let Some(val) = expr.strip_prefix("type=") {
        return Some(Filter::TypeEq(val));
    }
    // Try name~
    if let Some(val) = expr.strip_prefix("name~") {
        return Some(Filter::NameContains(val));
    }
    // Try id=
    if let Some(val) = expr.strip_prefix("id=") {
        let _ = parse_id(val)?; // validate
        return Some(Filter::IdEq(parse_id(val).unwrap()));
    }
    // Try content~
    if let Some(val) = expr.strip_prefix("content~") {
        return Some(Filter::ContentContains(val));
    }
    // Try edges>
    if let Some(val) = expr.strip_prefix("edges>") {
        let n = parse_usize(val)?;
        return Some(Filter::EdgesGt(n));
    }
    // Try edges=
    if let Some(val) = expr.strip_prefix("edges=") {
        let n = parse_usize(val)?;
        return Some(Filter::EdgesEq(n));
    }
    None
}

fn parse_id(s: &str) -> Option<u64> {
    s.trim().parse::<u64>().ok()
}

fn parse_usize(s: &str) -> Option<usize> {
    s.trim().parse::<usize>().ok()
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

fn matches_filter(node: &Node, filter: &Filter<'_>) -> bool {
    match filter {
        Filter::TypeEq(t) => {
            let type_str = match node.type_tag {
                NodeType::Text => "text",
                NodeType::Binary => "binary",
                NodeType::Config => "config",
                NodeType::System => "system",
                NodeType::Directory => "dir",
                NodeType::Computed => "computed",
                NodeType::Channel => "channel",
            };
            type_str == *t
        }
        Filter::NameContains(sub) => contains(&node.name, sub),
        Filter::IdEq(id) => node.id == *id,
        Filter::ContentContains(sub) => {
            match core::str::from_utf8(&node.content) {
                Ok(s) => contains(s, sub),
                Err(_) => false,
            }
        }
        Filter::EdgesGt(n) => node.edges.len() > *n,
        Filter::EdgesEq(n) => node.edges.len() == *n,
    }
}

/// Simple substring search (no regex).
fn contains(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.len() > h.len() {
        return false;
    }
    for i in 0..=(h.len() - n.len()) {
        let mut matched = true;
        for j in 0..n.len() {
            // Case-insensitive comparison
            let a = to_lower(h[i + j]);
            let b = to_lower(n[j]);
            if a != b {
                matched = false;
                break;
            }
        }
        if matched {
            return true;
        }
    }
    false
}

fn to_lower(b: u8) -> u8 {
    if b >= b'A' && b <= b'Z' {
        b + 32
    } else {
        b
    }
}

fn filter_nodes<'a>(graph: &'a Graph, filter: &Filter<'_>) -> Vec<&'a Node> {
    graph.nodes.values().filter(|n| matches_filter(n, filter)).collect()
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Execute a GQL query string and print results.
pub fn execute(input: &str, graph: &Graph) {
    // Check for pipe composition
    if let Some(pipe_pos) = find_pipe(input) {
        let left = input[..pipe_pos].trim();
        let right = input[pipe_pos + 1..].trim();
        execute_pipe(left, right, graph);
        return;
    }

    let query = match parse_query(input) {
        Some(q) => q,
        None => {
            crate::println!("Invalid query. Examples:");
            crate::println!("  gql type=system      - filter by type");
            crate::println!("  gql name~mem         - name substring match");
            crate::println!("  gql edges>2          - nodes with >2 edges");
            crate::println!("  gql children 1       - children of node 1");
            crate::println!("  gql parent 5         - parents of node 5");
            crate::println!("  gql descendants 1    - all descendants of node 1");
            crate::println!("  gql path 1 5         - shortest path");
            crate::println!("  gql count type=system- count matching nodes");
            return;
        }
    };

    match query {
        Query::Filter(filter) => {
            let results = filter_nodes(graph, &filter);
            if results.is_empty() {
                crate::println!("(no results)");
            } else {
                for node in &results {
                    crate::println!("  #{}   {}   {}", node.id, node.type_tag, node.name);
                }
                crate::println!("({} result{})", results.len(), if results.len() == 1 { "" } else { "s" });
            }
        }
        Query::Count(filter_opt) => {
            match filter_opt {
                Some(filter) => {
                    let count = graph.nodes.values().filter(|n| matches_filter(n, &filter)).count();
                    crate::println!("{}", count);
                }
                None => {
                    crate::println!("{}", graph.node_count());
                }
            }
        }
        Query::Children(id, label_filter) => {
            let node = match graph.get_node(id) {
                Some(n) => n,
                None => {
                    crate::println!("Node #{} not found", id);
                    return;
                }
            };
            let mut count = 0usize;
            for edge in &node.edges {
                if let Some(lf) = label_filter {
                    if edge.label.as_str() != lf {
                        continue;
                    }
                }
                let target_type = graph.get_node(edge.target)
                    .map(|n| format!("{}", n.type_tag))
                    .unwrap_or_else(|| String::from("???"));
                let target_name = graph.get_node(edge.target)
                    .map(|n| n.name.as_str())
                    .unwrap_or("???");
                crate::println!("  --{}--> #{} {} ({})", edge.label, edge.target, target_name, target_type);
                count += 1;
            }
            if count == 0 {
                crate::println!("(no children)");
            } else {
                crate::println!("({} child{})", count, if count == 1 { "" } else { "ren" });
            }
        }
        Query::Parent(target_id) => {
            if graph.get_node(target_id).is_none() {
                crate::println!("Node #{} not found", target_id);
                return;
            }
            let mut count = 0usize;
            for node in graph.nodes.values() {
                for edge in &node.edges {
                    if edge.target == target_id {
                        crate::println!("  #{} {} --{}--> #{}", node.id, node.name, edge.label, target_id);
                        count += 1;
                    }
                }
            }
            if count == 0 {
                crate::println!("(no parents)");
            } else {
                crate::println!("({} parent{})", count, if count == 1 { "" } else { "s" });
            }
        }
        Query::Descendants(id) => {
            if graph.get_node(id).is_none() {
                crate::println!("Node #{} not found", id);
                return;
            }
            let descendants = bfs_descendants(graph, id);
            if descendants.is_empty() {
                crate::println!("(no descendants)");
            } else {
                for node in &descendants {
                    crate::println!("  #{}   {}   {}", node.id, node.type_tag, node.name);
                }
                crate::println!("({} descendant{})", descendants.len(), if descendants.len() == 1 { "" } else { "s" });
            }
        }
        Query::Path(from, to) => {
            if graph.get_node(from).is_none() {
                crate::println!("Node #{} not found", from);
                return;
            }
            if graph.get_node(to).is_none() {
                crate::println!("Node #{} not found", to);
                return;
            }
            match bfs_path(graph, from, to) {
                Some(path) => {
                    print_path(graph, &path);
                }
                None => {
                    crate::println!("No path from #{} to #{}", from, to);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// BFS helpers
// ---------------------------------------------------------------------------

fn bfs_descendants<'a>(graph: &'a Graph, start: u64) -> Vec<&'a Node> {
    let mut visited: Vec<u64> = Vec::new();
    let mut queue: Vec<u64> = Vec::new();
    let mut result: Vec<&'a Node> = Vec::new();

    // Seed with direct children
    if let Some(node) = graph.get_node(start) {
        for edge in &node.edges {
            if !visited.contains(&edge.target) {
                visited.push(edge.target);
                queue.push(edge.target);
            }
        }
    }

    let mut head = 0;
    while head < queue.len() {
        let id = queue[head];
        head += 1;

        if let Some(node) = graph.get_node(id) {
            result.push(node);
            for edge in &node.edges {
                if !visited.contains(&edge.target) && edge.target != start {
                    visited.push(edge.target);
                    queue.push(edge.target);
                }
            }
        }
    }

    result
}

/// BFS shortest path. Returns list of (node_id, edge_label_to_next) pairs.
fn bfs_path(graph: &Graph, from: u64, to: u64) -> Option<Vec<(u64, String)>> {
    if from == to {
        let mut path = Vec::new();
        path.push((from, String::new()));
        return Some(path);
    }

    // BFS: store (node_id, parent_index, edge_label_from_parent)
    let mut visited: Vec<u64> = Vec::new();
    let mut queue: Vec<(u64, Option<usize>, String)> = Vec::new();

    visited.push(from);
    queue.push((from, None, String::new()));

    let mut head = 0;
    while head < queue.len() {
        let (current_id, _, _) = (queue[head].0, queue[head].1, &queue[head].2 as *const _);
        head += 1;

        if let Some(node) = graph.get_node(current_id) {
            for edge in &node.edges {
                if !visited.contains(&edge.target) {
                    visited.push(edge.target);
                    queue.push((edge.target, Some(head - 1), edge.label.clone()));

                    if edge.target == to {
                        // Reconstruct path
                        let mut path = Vec::new();
                        let mut idx = queue.len() - 1;
                        loop {
                            let (nid, parent, label) = &queue[idx];
                            path.push((*nid, label.clone()));
                            match parent {
                                Some(pidx) => idx = *pidx,
                                None => break,
                            }
                        }
                        path.reverse();
                        return Some(path);
                    }
                }
            }
        }
    }

    None
}

fn print_path(graph: &Graph, path: &[(u64, String)]) {
    let mut output = String::new();
    for (i, (id, label)) in path.iter().enumerate() {
        if i > 0 {
            output.push_str(&format!(" --{}--> ", label));
        }
        let name = graph.get_node(*id).map(|n| n.name.as_str()).unwrap_or("???");
        output.push_str(&format!("#{} {}", id, name));
    }
    crate::println!("  {}", output);
    let edge_count = if path.len() > 1 { path.len() - 1 } else { 0 };
    crate::println!("  (path length: {})", edge_count);
}

// ---------------------------------------------------------------------------
// Pipe composition
// ---------------------------------------------------------------------------

fn find_pipe(input: &str) -> Option<usize> {
    let bytes = input.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'|' {
            return Some(i);
        }
    }
    None
}

fn execute_pipe(left: &str, right: &str, graph: &Graph) {
    // Parse left as filter
    let left_filter = match parse_filter(left.trim()) {
        Some(f) => f,
        None => {
            crate::println!("Invalid left-side filter: {}", left);
            return;
        }
    };

    let right_filter = match parse_filter(right.trim()) {
        Some(f) => f,
        None => {
            crate::println!("Invalid right-side filter: {}", right);
            return;
        }
    };

    let results: Vec<&Node> = graph.nodes.values()
        .filter(|n| matches_filter(n, &left_filter) && matches_filter(n, &right_filter))
        .collect();

    if results.is_empty() {
        crate::println!("(no results)");
    } else {
        for node in &results {
            crate::println!("  #{}   {}   {}", node.id, node.type_tag, node.name);
        }
        crate::println!("({} result{})", results.len(), if results.len() == 1 { "" } else { "s" });
    }
}
