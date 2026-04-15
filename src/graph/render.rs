/// Graph visualization renderer — tree layout for the ramfb framebuffer.
///
/// Renders the graph as a top-down tree rooted at node #1. Nodes are
/// labeled boxes with colored borders (by type), connected by Manhattan-
/// routed edge lines. Layout uses a recursive subtree-width algorithm
/// with no floating point.

use crate::framebuffer::{Framebuffer, Pixel, draw_string};
use super::{Graph, NodeType};
use super::navigator::NavigatorState;
use alloc::vec::Vec;
use alloc::string::String;
use alloc::format;
use alloc::collections::BTreeSet;

// ---------------------------------------------------------------------------
// Color scheme — solar theme
// ---------------------------------------------------------------------------

const BG: Pixel           = Pixel::new(0x1a, 0x1a, 0x2e); // dark indigo
const TITLE_COLOR: Pixel  = Pixel::new(0xf0, 0xa5, 0x00); // golden amber
const CARD_BG: Pixel      = Pixel::new(0x25, 0x25, 0x47); // lighter indigo
const NODE_NAME_C: Pixel  = Pixel::new(0xff, 0xff, 0xff); // white
const NODE_TYPE_C: Pixel  = Pixel::new(0x88, 0x88, 0x99); // dim
const EDGE_LINE_C: Pixel  = Pixel::new(0x55, 0x66, 0x88); // edge lines
const EDGE_LABEL_C: Pixel = Pixel::new(0x66, 0xcc, 0xff); // edge label text
const SUMMARY_C: Pixel    = Pixel::new(0x88, 0x88, 0x99); // summary text
const SEP_C: Pixel        = Pixel::new(0x44, 0x44, 0x66); // separator line

const HIGHLIGHT_C: Pixel  = Pixel::new(0x00, 0xff, 0xaa); // bright cyan-green
const DETAIL_BG: Pixel    = Pixel::new(0x20, 0x20, 0x40); // detail panel bg
const DETAIL_BORDER: Pixel= Pixel::new(0x44, 0x55, 0x77); // detail panel border
const DETAIL_LABEL_C: Pixel = Pixel::new(0x88, 0x99, 0xbb); // detail labels
const DETAIL_VALUE_C: Pixel = Pixel::new(0xff, 0xff, 0xff); // detail values
const COLLAPSE_IND_C: Pixel = Pixel::new(0xff, 0xcc, 0x00); // [+] indicator

fn border_color(nt: NodeType) -> Pixel {
    match nt {
        NodeType::System    => Pixel::new(0xf0, 0xa5, 0x00), // gold
        NodeType::Directory => Pixel::new(0x66, 0xcc, 0xff), // blue
        NodeType::Text      => Pixel::new(0x66, 0xff, 0x88), // green
        NodeType::Config    => Pixel::new(0xff, 0x88, 0x44), // orange
        NodeType::Binary    => Pixel::new(0xaa, 0x66, 0xff), // purple
        NodeType::Computed  => Pixel::new(0x00, 0xcc, 0xaa), // teal/cyan
        NodeType::Channel   => Pixel::new(0xff, 0x66, 0xcc), // magenta/pink
    }
}

// ---------------------------------------------------------------------------
// Layout constants
// ---------------------------------------------------------------------------

const SCALE: u32 = 2;                    // text scale (16 px)
const CHAR_W: u32 = 8 * SCALE + SCALE;   // 18 px per char (with gap)
const CHAR_H: u32 = 8 * SCALE;           // 16 px tall

const SCALE_S: u32 = 1;                  // small text scale (8 px)
const CHAR_W_S: u32 = 8 + 1;             // 9 px per char (small)
const CHAR_H_S: u32 = 8;                 // 8 px tall

const NODE_H: u32 = 46;
const NODE_PAD: u32 = 10;
const MIN_NODE_W: u32 = 100;
const LEVEL_GAP: u32 = 80;               // vertical gap between levels
const SIBLING_GAP: u32 = 20;             // horizontal gap between siblings
const TITLE_AREA: u32 = 50;              // reserved for title + separator
const MARGIN: u32 = 30;

/// Compute the box width for a node based on its name length at SCALE.
fn node_box_width(name_len: usize) -> u32 {
    let text_w = name_len as u32 * CHAR_W;
    (text_w + 2 * NODE_PAD).max(MIN_NODE_W)
}

// ---------------------------------------------------------------------------
// Tree data structures
// ---------------------------------------------------------------------------

struct TreeNode {
    node_id: u64,
    children: Vec<TreeNode>,
    edge_labels: Vec<usize>, // index into a label pool (parallel to children)
}

struct Positioned {
    node_id: u64,
    cx: i32,  // center-x
    y: i32,   // top-y
    w: u32,   // box width
}

struct Edge {
    from_cx: i32,
    from_bot: i32,
    to_cx: i32,
    to_top: i32,
    label_idx: usize, // index into label pool; usize::MAX = no label
}

// ---------------------------------------------------------------------------
// Tree construction — follow "child" edges from root
// ---------------------------------------------------------------------------

fn collect_tree(graph: &Graph, root_id: u64, visited: &mut Vec<u64>, labels: &mut Vec<u8>) -> Option<TreeNode> {
    collect_tree_inner(graph, root_id, visited, labels, &BTreeSet::new())
}

fn collect_tree_inner(graph: &Graph, root_id: u64, visited: &mut Vec<u64>, labels: &mut Vec<u8>, collapsed: &BTreeSet<u64>) -> Option<TreeNode> {
    if visited.contains(&root_id) {
        return None;
    }
    let node = graph.get_node(root_id)?;
    visited.push(root_id);

    let mut children = Vec::new();
    let mut edge_labels = Vec::new();

    // If this node is collapsed, return it with no children
    if collapsed.contains(&root_id) {
        return Some(TreeNode { node_id: root_id, children, edge_labels });
    }

    for edge in &node.edges {
        if edge.label.as_str() == "child" {
            if let Some(child) = collect_tree_inner(graph, edge.target, visited, labels, collapsed) {
                // Store the label start index (we skip storing "child" labels to
                // keep the display clean — they're all "child" anyway)
                edge_labels.push(usize::MAX);
                children.push(child);
            }
        }
    }

    // Non-child edges (store labels for later display)
    for edge in &node.edges {
        if edge.label.as_str() != "child" && !visited.contains(&edge.target) {
            if let Some(child) = collect_tree_inner(graph, edge.target, visited, labels, collapsed) {
                let idx = labels.len();
                for b in edge.label.bytes() {
                    labels.push(b);
                }
                labels.push(0); // null terminator
                edge_labels.push(idx);
                children.push(child);
            }
        }
    }

    Some(TreeNode { node_id: root_id, children, edge_labels })
}

// ---------------------------------------------------------------------------
// Subtree width computation (bottom-up)
// ---------------------------------------------------------------------------

fn subtree_width(tree: &TreeNode, graph: &Graph) -> u32 {
    let nw = graph.get_node(tree.node_id)
        .map(|n| node_box_width(n.name.len()))
        .unwrap_or(MIN_NODE_W);

    if tree.children.is_empty() {
        return nw;
    }

    let mut total: u32 = 0;
    for (i, child) in tree.children.iter().enumerate() {
        if i > 0 {
            total += SIBLING_GAP;
        }
        total += subtree_width(child, graph);
    }

    total.max(nw)
}

// ---------------------------------------------------------------------------
// Position assignment (top-down)
// ---------------------------------------------------------------------------

fn layout(
    tree: &TreeNode,
    graph: &Graph,
    cx: i32,
    y: i32,
    nodes: &mut Vec<Positioned>,
    edges: &mut Vec<Edge>,
) {
    let nw = graph.get_node(tree.node_id)
        .map(|n| node_box_width(n.name.len()))
        .unwrap_or(MIN_NODE_W);

    nodes.push(Positioned { node_id: tree.node_id, cx, y, w: nw });

    if tree.children.is_empty() {
        return;
    }

    // Compute child subtree widths
    let cwidths: Vec<u32> = tree.children.iter()
        .map(|c| subtree_width(c, graph))
        .collect();
    let total_w: u32 = cwidths.iter().copied().sum::<u32>()
        + (tree.children.len() as u32).saturating_sub(1) * SIBLING_GAP;

    let child_y = y + NODE_H as i32 + LEVEL_GAP as i32;
    let mut child_left = cx - total_w as i32 / 2;

    for (i, child) in tree.children.iter().enumerate() {
        let cw = cwidths[i];
        let child_cx = child_left + cw as i32 / 2;

        edges.push(Edge {
            from_cx: cx,
            from_bot: y + NODE_H as i32,
            to_cx: child_cx,
            to_top: child_y,
            label_idx: tree.edge_labels[i],
        });

        layout(child, graph, child_cx, child_y, nodes, edges);

        child_left += cw as i32 + SIBLING_GAP as i32;
    }
}

// ---------------------------------------------------------------------------
// Drawing helpers (signed coordinates, clipped to screen)
// ---------------------------------------------------------------------------

fn draw_vline_s(fb: &Framebuffer, x: i32, y0: i32, y1: i32, color: Pixel) {
    if x < 0 || x >= fb.width as i32 { return; }
    let (ya, yb) = if y0 <= y1 { (y0, y1) } else { (y1, y0) };
    let ya = ya.max(0) as u32;
    let yb = yb.min(fb.height as i32 - 1) as u32;
    if ya <= yb {
        fb.draw_vline(x as u32, ya, yb - ya + 1, color);
    }
}

fn draw_hline_s(fb: &Framebuffer, x0: i32, x1: i32, y: i32, color: Pixel) {
    if y < 0 || y >= fb.height as i32 { return; }
    let (xa, xb) = if x0 <= x1 { (x0, x1) } else { (x1, x0) };
    let xa = xa.max(0) as u32;
    let xb = xb.min(fb.width as i32 - 1) as u32;
    if xa <= xb {
        fb.draw_hline(xa, y as u32, xb - xa + 1, color);
    }
}

// ---------------------------------------------------------------------------
// Edge rendering — Manhattan routing
// ---------------------------------------------------------------------------

fn draw_edge(fb: &Framebuffer, e: &Edge, _labels: &[u8]) {
    let mid_y = (e.from_bot + e.to_top) / 2;

    // Vertical: parent bottom → mid
    draw_vline_s(fb, e.from_cx, e.from_bot, mid_y, EDGE_LINE_C);

    // Horizontal: parent cx → child cx at mid
    draw_hline_s(fb, e.from_cx, e.to_cx, mid_y, EDGE_LINE_C);

    // Vertical: mid → child top
    draw_vline_s(fb, e.to_cx, mid_y, e.to_top, EDGE_LINE_C);

    // Optional edge label (only for non-"child" edges)
    if e.label_idx != usize::MAX && e.label_idx < _labels.len() {
        // Extract null-terminated label
        let start = e.label_idx;
        let mut end = start;
        while end < _labels.len() && _labels[end] != 0 {
            end += 1;
        }
        if end > start {
            if let Ok(label) = core::str::from_utf8(&_labels[start..end]) {
                let lx = (e.from_cx + e.to_cx) / 2 - (label.len() as i32 * CHAR_W_S as i32) / 2;
                let ly = mid_y - CHAR_H_S as i32 - 2;
                if lx >= 0 && ly >= 0 {
                    draw_string(fb, label, lx as u32, ly as u32, SCALE_S, EDGE_LABEL_C);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Node rendering
// ---------------------------------------------------------------------------

fn draw_node_box(fb: &Framebuffer, graph: &Graph, pn: &Positioned) {
    draw_node_box_ex(fb, graph, pn, false, false);
}

fn draw_node_box_ex(fb: &Framebuffer, graph: &Graph, pn: &Positioned, selected: bool, is_collapsed: bool) {
    let node = match graph.get_node(pn.node_id) {
        Some(n) => n,
        None => {
            crate::println!("[render] node #{} not found in graph!", pn.node_id);
            return;
        }
    };

    let left = pn.cx - pn.w as i32 / 2;
    let top = pn.y;

    // Off-screen check
    if left + pn.w as i32 <= 0 || left >= fb.width as i32 {
        crate::println!("[render] node #{} '{}' off-screen X: left={}, w={}, fb_w={}", pn.node_id, node.name, left, pn.w, fb.width);
        return;
    }
    if top + NODE_H as i32 <= 0 || top >= fb.height as i32 {
        crate::println!("[render] node #{} '{}' off-screen Y: top={}, fb_h={}", pn.node_id, node.name, top, fb.height);
        return;
    }

    let ux = left.max(0) as u32;
    let uy = top.max(0) as u32;

    crate::println!("[render] node #{} '{}': left={}, top={}, ux={}, uy={}, w={}, sel={}", pn.node_id, node.name, left, top, ux, uy, pn.w, selected);

    // Fill background
    fb.fill_rect(ux, uy, pn.w, NODE_H, CARD_BG);

    if selected {
        // Bright highlight border (3 px thick)
        fb.draw_rect_outline(ux, uy, pn.w, NODE_H, HIGHLIGHT_C);
        if pn.w > 4 && NODE_H > 4 {
            fb.draw_rect_outline(ux + 1, uy + 1, pn.w - 2, NODE_H - 2, HIGHLIGHT_C);
        }
        if pn.w > 6 && NODE_H > 6 {
            fb.draw_rect_outline(ux + 2, uy + 2, pn.w - 4, NODE_H - 4, HIGHLIGHT_C);
        }
    } else {
        // Colored border (double thickness for emphasis)
        let bc = border_color(node.type_tag);
        fb.draw_rect_outline(ux, uy, pn.w, NODE_H, bc);
        if pn.w > 4 && NODE_H > 4 {
            fb.draw_rect_outline(ux + 1, uy + 1, pn.w - 2, NODE_H - 2, bc);
        }
    }

    // Name text (centered, SCALE=2)
    let name = &node.name;
    // If collapsed, prepend [+]
    let display_name = if is_collapsed {
        format!("[+] {}", name)
    } else {
        String::from(name.as_str())
    };
    let name_w = display_name.len() as u32 * CHAR_W;
    let name_x = ux + pn.w.saturating_sub(name_w) / 2;
    let name_y = uy + 5;
    if is_collapsed {
        // Draw [+] in indicator color, rest in normal
        let prefix = "[+] ";
        let prefix_w = prefix.len() as u32 * CHAR_W;
        draw_string(fb, prefix, name_x, name_y, SCALE, COLLAPSE_IND_C);
        draw_string(fb, name, name_x + prefix_w, name_y, SCALE, NODE_NAME_C);
    } else {
        draw_string(fb, &display_name, name_x, name_y, SCALE, NODE_NAME_C);
    }

    // Type text (centered, SCALE=1 — smaller)
    let type_str = format!("({})", node.type_tag);
    let type_w = type_str.len() as u32 * CHAR_W_S;
    let type_x = ux + pn.w.saturating_sub(type_w) / 2;
    let type_y = name_y + CHAR_H + 4;
    draw_string(fb, &type_str, type_x, type_y, SCALE_S, NODE_TYPE_C);
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Render the graph onto the framebuffer as a top-down tree.
pub fn render(fb: &Framebuffer, graph: &Graph) {
    // 1. Clear background
    fb.fill(BG);

    // 2. Title bar
    let title = "HELIOS - Graph Memory";
    let title_w = title.len() as u32 * CHAR_W;
    let title_x = (fb.width.saturating_sub(title_w)) / 2;
    draw_string(fb, title, title_x, 10, SCALE, TITLE_COLOR);

    // Separator line
    let sep_y = 10 + CHAR_H + 8;
    let sep_w = title_w + 40;
    let sep_x = (fb.width.saturating_sub(sep_w)) / 2;
    fb.draw_hline(sep_x, sep_y, sep_w, SEP_C);

    // Hint bar below title
    let hint = "type \"nav\" for interactive | type \"tty\" for terminal";
    let hint_w = hint.len() as u32 * CHAR_W_S;
    let hint_x = (fb.width.saturating_sub(hint_w)) / 2;
    draw_string(fb, hint, hint_x, sep_y + 4, SCALE_S, SUMMARY_C);

    // 3. Build tree from root (node #1)
    let mut visited = Vec::new();
    let mut label_pool: Vec<u8> = Vec::new();
    let tree = match collect_tree(graph, 1, &mut visited, &mut label_pool) {
        Some(t) => t,
        None => {
            draw_string(fb, "(no root node)", MARGIN, TITLE_AREA, SCALE, SUMMARY_C);
            return;
        }
    };

    // 4. Layout positions
    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    let root_cx = fb.width as i32 / 2;
    let root_y = TITLE_AREA as i32 + 6;

    layout(&tree, graph, root_cx, root_y, &mut nodes, &mut edges);

    // 5. Draw edges first (behind nodes)
    for e in &edges {
        draw_edge(fb, e, &label_pool);
    }

    // 6. Draw nodes on top
    for pn in &nodes {
        draw_node_box(fb, graph, pn);
    }

    // 7. Summary bar at bottom
    let summary = format!("{} nodes, {} edges", graph.node_count(), graph.edge_count());
    let summary_y = fb.height - MARGIN - CHAR_H;
    draw_string(fb, &summary, MARGIN, summary_y, SCALE, SUMMARY_C);

    // Node count on right side
    let visited_str = format!("{} in tree", visited.len());
    let vis_w = visited_str.len() as u32 * CHAR_W;
    let vis_x = fb.width - MARGIN - vis_w;
    draw_string(fb, &visited_str, vis_x, summary_y, SCALE, SUMMARY_C);
}

// ---------------------------------------------------------------------------
// Navigator-aware render
// ---------------------------------------------------------------------------

/// Detail panel width (pixels)
const DETAIL_W: u32 = 300;
const DETAIL_PAD: u32 = 12;
const DETAIL_LINE_H: u32 = 14; // line spacing for small text

/// Render the graph with navigator state: highlights, collapse indicators, detail panel.
pub fn render_navigated(fb: &Framebuffer, graph: &Graph, nav: &NavigatorState) {
    // 1. Clear background
    fb.fill(BG);

    // 2. Title bar
    let title = "HELIOS - Graph Navigator";
    let title_w = title.len() as u32 * CHAR_W;
    let title_x = (fb.width.saturating_sub(title_w)) / 2;
    draw_string(fb, title, title_x, 10, SCALE, TITLE_COLOR);

    // Separator line
    let sep_y = 10 + CHAR_H + 8;
    let sep_w = title_w + 40;
    let sep_x = (fb.width.saturating_sub(sep_w)) / 2;
    fb.draw_hline(sep_x, sep_y, sep_w, SEP_C);

    // Hint bar below title
    let hint = "[Arrows:nav] [Enter:expand] [d:detail] [t:terminal] [q:quit]";
    let hint_w = hint.len() as u32 * CHAR_W_S;
    let hint_x = (fb.width.saturating_sub(hint_w)) / 2;
    draw_string(fb, hint, hint_x, sep_y + 4, SCALE_S, SUMMARY_C);

    // 3. Build tree from root with collapsed set
    let mut visited = Vec::new();
    let mut label_pool: Vec<u8> = Vec::new();
    let tree = match collect_tree_inner(graph, 1, &mut visited, &mut label_pool, &nav.collapsed) {
        Some(t) => t,
        None => {
            draw_string(fb, "(no root node)", MARGIN, TITLE_AREA, SCALE, SUMMARY_C);
            return;
        }
    };

    // 4. Compute available width for tree (leave room for detail panel if shown)
    let tree_area_w = if nav.detail_panel {
        fb.width.saturating_sub(DETAIL_W + MARGIN)
    } else {
        fb.width
    };

    // 5. Layout positions
    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    let root_cx = tree_area_w as i32 / 2;
    let root_y = TITLE_AREA as i32 + 20;

    layout(&tree, graph, root_cx, root_y, &mut nodes, &mut edges);

    // 6. Draw edges first (behind nodes)
    for e in &edges {
        draw_edge(fb, e, &label_pool);
    }

    // 7. Draw nodes on top, with selection highlight and collapse indicator
    for pn in &nodes {
        let is_selected = pn.node_id == nav.selected_node;
        let is_collapsed = nav.collapsed.contains(&pn.node_id);
        draw_node_box_ex(fb, graph, pn, is_selected, is_collapsed);
    }

    // 8. Detail panel (right side)
    if nav.detail_panel {
        draw_detail_panel(fb, graph, nav.selected_node);
    }

    // 9. Summary bar at bottom
    let summary = format!("{} nodes | selected: #{}", graph.node_count(), nav.selected_node);
    let summary_y = fb.height - MARGIN - CHAR_H;
    draw_string(fb, &summary, MARGIN, summary_y, SCALE, SUMMARY_C);
}

/// Draw the detail panel on the right side for the given node.
fn draw_detail_panel(fb: &Framebuffer, graph: &Graph, node_id: u64) {
    let panel_x = fb.width - DETAIL_W - MARGIN / 2;
    let panel_y = TITLE_AREA + 20;
    let panel_h = fb.height - panel_y - MARGIN - CHAR_H - 10;

    // Panel background
    fb.fill_rect(panel_x, panel_y, DETAIL_W, panel_h, DETAIL_BG);
    fb.draw_rect_outline(panel_x, panel_y, DETAIL_W, panel_h, DETAIL_BORDER);

    let cx = panel_x + DETAIL_PAD;
    let mut cy = panel_y + DETAIL_PAD;
    let max_y = panel_y + panel_h - DETAIL_PAD;
    let text_max_chars = ((DETAIL_W - 2 * DETAIL_PAD) / CHAR_W_S) as usize;

    // Title
    draw_string(fb, "Node Details", cx, cy, SCALE_S, HIGHLIGHT_C);
    cy += DETAIL_LINE_H + 4;

    // Separator
    fb.draw_hline(cx, cy, DETAIL_W - 2 * DETAIL_PAD, SEP_C);
    cy += 6;

    let node = match graph.get_node(node_id) {
        Some(n) => n,
        None => {
            draw_string(fb, "(node not found)", cx, cy, SCALE_S, DETAIL_VALUE_C);
            return;
        }
    };

    // ID
    if cy + DETAIL_LINE_H > max_y { return; }
    let id_str = format!("ID: {}", node.id);
    draw_detail_line(fb, cx, cy, &id_str, text_max_chars);
    cy += DETAIL_LINE_H;

    // Type
    if cy + DETAIL_LINE_H > max_y { return; }
    let type_str = format!("Type: {}", node.type_tag);
    draw_detail_line(fb, cx, cy, &type_str, text_max_chars);
    cy += DETAIL_LINE_H;

    // Name
    if cy + DETAIL_LINE_H > max_y { return; }
    let name_str = format!("Name: {}", node.name);
    draw_detail_line(fb, cx, cy, &name_str, text_max_chars);
    cy += DETAIL_LINE_H + 4;

    // Separator
    if cy + 6 > max_y { return; }
    fb.draw_hline(cx, cy, DETAIL_W - 2 * DETAIL_PAD, SEP_C);
    cy += 6;

    // Content
    if cy + DETAIL_LINE_H > max_y { return; }
    draw_string(fb, "Content:", cx, cy, SCALE_S, DETAIL_LABEL_C);
    cy += DETAIL_LINE_H;

    if node.type_tag == NodeType::Computed {
        // Show formula
        if cy + DETAIL_LINE_H > max_y { return; }
        let formula = core::str::from_utf8(&node.content).unwrap_or("(invalid)");
        let f_str = format!("F: {}", formula);
        draw_detail_line(fb, cx, cy, &f_str, text_max_chars);
        cy += DETAIL_LINE_H;

        // Show result
        if cy + DETAIL_LINE_H > max_y { return; }
        let result = node.display_content(graph);
        let r_str = format!("= {}", result);
        draw_detail_line(fb, cx, cy, &r_str, text_max_chars);
        cy += DETAIL_LINE_H;
    } else if node.content.is_empty() {
        if cy + DETAIL_LINE_H > max_y { return; }
        draw_string(fb, "(empty)", cx, cy, SCALE_S, DETAIL_VALUE_C);
        cy += DETAIL_LINE_H;
    } else {
        // Show content, multi-line if needed
        match core::str::from_utf8(&node.content) {
            Ok(s) => {
                for line in s.split('\n') {
                    if cy + DETAIL_LINE_H > max_y { break; }
                    draw_detail_line(fb, cx, cy, line, text_max_chars);
                    cy += DETAIL_LINE_H;
                }
            }
            Err(_) => {
                if cy + DETAIL_LINE_H > max_y { return; }
                let bin_str = format!("({} bytes, binary)", node.content.len());
                draw_detail_line(fb, cx, cy, &bin_str, text_max_chars);
                cy += DETAIL_LINE_H;
            }
        }
    }

    cy += 4;

    // Separator
    if cy + 6 > max_y { return; }
    fb.draw_hline(cx, cy, DETAIL_W - 2 * DETAIL_PAD, SEP_C);
    cy += 6;

    // Edges
    if cy + DETAIL_LINE_H > max_y { return; }
    let edge_hdr = format!("Edges ({})", node.edges.len());
    draw_string(fb, &edge_hdr, cx, cy, SCALE_S, DETAIL_LABEL_C);
    cy += DETAIL_LINE_H;

    if node.edges.is_empty() {
        if cy + DETAIL_LINE_H <= max_y {
            draw_string(fb, "(none)", cx, cy, SCALE_S, DETAIL_VALUE_C);
        }
    } else {
        for edge in &node.edges {
            if cy + DETAIL_LINE_H > max_y { break; }
            let target_name = graph.get_node(edge.target)
                .map(|n| n.name.as_str())
                .unwrap_or("???");
            let edge_str = format!("{} -> #{} {}", edge.label, edge.target, target_name);
            draw_detail_line(fb, cx, cy, &edge_str, text_max_chars);
            cy += DETAIL_LINE_H;
        }
    }
}

/// Draw a detail line, truncating if longer than max_chars.
fn draw_detail_line(fb: &Framebuffer, x: u32, y: u32, text: &str, max_chars: usize) {
    if text.len() <= max_chars {
        draw_string(fb, text, x, y, SCALE_S, DETAIL_VALUE_C);
    } else {
        // Truncate with ".."
        let truncated = &text[..max_chars.saturating_sub(2)];
        let display = format!("{}..", truncated);
        draw_string(fb, &display, x, y, SCALE_S, DETAIL_VALUE_C);
    }
}
