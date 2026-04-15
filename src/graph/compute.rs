/// Reactive graph node formula evaluation.
///
/// Computed nodes store a formula as their content. When read, the formula
/// is evaluated against the current graph state to produce a dynamic result.

use alloc::format;
use alloc::string::String;
use crate::arch::riscv64 as arch;
use crate::alloc_impl;
use super::{Graph, NodeType};

/// Timer frequency on QEMU virt (10 MHz).
const TIMER_FREQ: usize = 10_000_000;

/// Maximum recursion depth for template evaluation (prevents infinite loops).
const MAX_DEPTH: usize = 3;

/// Evaluate a computed node's formula and return the result as a string.
pub fn evaluate(formula: &str, graph: &Graph) -> String {
    evaluate_inner(formula, graph, 0)
}

fn evaluate_inner(formula: &str, graph: &Graph, depth: usize) -> String {
    if depth > MAX_DEPTH {
        return String::from("(recursion limit)");
    }

    let formula = formula.trim();

    // $count(type) — count nodes of a given type
    if formula.starts_with("$count(") {
        if let Some(arg) = extract_parens(formula) {
            if arg == "all" {
                return format!("{}", graph.node_count());
            }
            if let Some(nt) = NodeType::from_str(arg) {
                let count = graph.nodes.values().filter(|n| n.type_tag == nt).count();
                return format!("{}", count);
            }
            return format!("(unknown type: {})", arg);
        }
    }

    // $sum(id1, id2, ...) — sum content of nodes as numbers
    if formula.starts_with("$sum(") {
        if let Some(arg) = extract_parens(formula) {
            let mut total: i64 = 0;
            for id_str in arg.split(',') {
                if let Some(id) = parse_id(id_str.trim()) {
                    if let Some(node) = graph.get_node(id) {
                        let content = if node.type_tag == NodeType::Computed {
                            evaluate_inner(
                                core::str::from_utf8(&node.content).unwrap_or(""),
                                graph,
                                depth + 1,
                            )
                        } else {
                            String::from(core::str::from_utf8(&node.content).unwrap_or("0"))
                        };
                        if let Ok(v) = content.trim().parse::<i64>() {
                            total += v;
                        }
                    }
                }
            }
            return format!("{}", total);
        }
    }

    // $concat(id1, id2, ...) — concatenate content
    if formula.starts_with("$concat(") {
        if let Some(arg) = extract_parens(formula) {
            let mut result = String::new();
            for id_str in arg.split(',') {
                if let Some(id) = parse_id(id_str.trim()) {
                    if let Some(node) = graph.get_node(id) {
                        if node.type_tag == NodeType::Computed {
                            result.push_str(&evaluate_inner(
                                core::str::from_utf8(&node.content).unwrap_or(""),
                                graph,
                                depth + 1,
                            ));
                        } else if let Ok(s) = core::str::from_utf8(&node.content) {
                            result.push_str(s);
                        }
                    }
                }
            }
            return result;
        }
    }

    // $edges(id) — count edges
    if formula.starts_with("$edges(") {
        if let Some(arg) = extract_parens(formula) {
            if let Some(id) = parse_id(arg) {
                if let Some(node) = graph.get_node(id) {
                    return format!("{}", node.edges.len());
                }
                return String::from("(node not found)");
            }
        }
    }

    // $children(id) — list child node names
    if formula.starts_with("$children(") {
        if let Some(arg) = extract_parens(formula) {
            if let Some(id) = parse_id(arg) {
                if let Some(node) = graph.get_node(id) {
                    let mut names = String::new();
                    let mut first = true;
                    for edge in &node.edges {
                        if edge.label.as_str() == "child" {
                            if let Some(child) = graph.get_node(edge.target) {
                                if !first {
                                    names.push_str(", ");
                                }
                                names.push_str(&child.name);
                                first = false;
                            }
                        }
                    }
                    return if names.is_empty() {
                        String::from("(no children)")
                    } else {
                        names
                    };
                }
                return String::from("(node not found)");
            }
        }
    }

    // $uptime — current uptime
    if formula == "$uptime" {
        return eval_uptime();
    }

    // $mem — memory usage summary
    if formula == "$mem" {
        return eval_mem();
    }

    // $graph — graph stats
    if formula == "$graph" {
        return format!("{} nodes, {} edges", graph.node_count(), graph.edge_count());
    }

    // $template{...} — template string with inline formulas
    if formula.starts_with("$template{") && formula.ends_with("}") {
        let inner = &formula[10..formula.len() - 1]; // strip "$template{" and "}"
        return eval_template(inner, graph, depth);
    }

    // Unknown formula — return as-is
    String::from(formula)
}

/// Evaluate a template string, replacing inline formula references.
fn eval_template(template: &str, graph: &Graph, depth: usize) -> String {
    let mut result = String::new();
    let bytes = template.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if bytes[i] == b'$' && i + 1 < len {
            // Try to match a formula pattern
            if let Some((replacement, consumed)) = try_match_inline(template, i, graph, depth) {
                result.push_str(&replacement);
                i += consumed;
                continue;
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }

    result
}

/// Try to match an inline formula starting at position `start` in the template.
/// Returns (replacement_text, bytes_consumed) or None.
fn try_match_inline(template: &str, start: usize, graph: &Graph, depth: usize) -> Option<(String, usize)> {
    let rest = &template[start..];

    // ${N} — interpolate node content by ID
    if rest.starts_with("${") {
        if let Some(end) = rest.find('}') {
            let id_str = &rest[2..end];
            if let Some(id) = parse_id(id_str) {
                if let Some(node) = graph.get_node(id) {
                    let content = if node.type_tag == NodeType::Computed {
                        evaluate_inner(
                            core::str::from_utf8(&node.content).unwrap_or(""),
                            graph,
                            depth + 1,
                        )
                    } else {
                        String::from(core::str::from_utf8(&node.content).unwrap_or(""))
                    };
                    return Some((content, end + 1));
                }
                return Some((String::from("(?)"), end + 1));
            }
        }
    }

    // $count(...), $sum(...), $concat(...), $edges(...), $children(...)
    for keyword in &["$count(", "$sum(", "$concat(", "$edges(", "$children("] {
        if rest.starts_with(keyword) {
            // Find matching closing paren
            if let Some(paren_end) = find_closing_paren(rest, keyword.len() - 1) {
                let formula_str = &rest[..paren_end + 1];
                let result = evaluate_inner(formula_str, graph, depth + 1);
                return Some((result, paren_end + 1));
            }
        }
    }

    // $uptime
    if rest.starts_with("$uptime") {
        // Make sure it's not part of a longer word
        let after = 7; // len("$uptime")
        if after >= rest.len() || !rest.as_bytes()[after].is_ascii_alphanumeric() {
            return Some((eval_uptime(), after));
        }
    }

    // $mem
    if rest.starts_with("$mem") {
        let after = 4;
        if after >= rest.len() || !rest.as_bytes()[after].is_ascii_alphanumeric() {
            return Some((eval_mem(), after));
        }
    }

    // $graph
    if rest.starts_with("$graph") {
        let after = 6;
        if after >= rest.len() || !rest.as_bytes()[after].is_ascii_alphanumeric() {
            let result = format!("{} nodes, {} edges", graph.node_count(), graph.edge_count());
            return Some((result, after));
        }
    }

    None
}

/// Find the closing ')' for a '(' at position `open_pos`.
fn find_closing_paren(s: &str, open_pos: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    if open_pos >= bytes.len() || bytes[open_pos] != b'(' {
        return None;
    }
    let mut depth = 1;
    let mut i = open_pos + 1;
    while i < bytes.len() {
        if bytes[i] == b'(' {
            depth += 1;
        } else if bytes[i] == b')' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Extract the content inside parentheses from a formula like "$count(system)".
fn extract_parens(formula: &str) -> Option<&str> {
    let open = formula.find('(')?;
    let close = formula.rfind(')')?;
    if close > open + 1 {
        Some(&formula[open + 1..close])
    } else {
        None
    }
}

/// Parse a string as a u64 node ID.
fn parse_id(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    s.parse::<u64>().ok()
}

fn eval_uptime() -> String {
    let time = arch::read_time();
    let secs = time / TIMER_FREQ;
    let frac = (time % TIMER_FREQ) / (TIMER_FREQ / 10);
    format!("{}.{}s", secs, frac)
}

fn eval_mem() -> String {
    let used = alloc_impl::heap_used();
    let total = alloc_impl::heap_total();
    format!("{}K / {}K", used / 1024, total / 1024)
}
