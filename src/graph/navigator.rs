/// Interactive graph navigator — keyboard-driven framebuffer graph exploration.
///
/// Provides a navigator mode where the user can browse the graph tree with
/// arrow keys, expand/collapse subtrees, and view node details in a side panel.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

/// Parsed navigator input actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavInput {
    Up,
    Down,
    Left,
    Right,
    ToggleCollapse,
    ToggleDetail,
    Refresh,
    Quit,
}

/// Navigator state — tracks selection, collapsed nodes, and detail panel visibility.
pub struct NavigatorState {
    pub selected_node: u64,
    pub collapsed: BTreeSet<u64>,
    pub detail_panel: bool,
}

/// A flattened visible node entry used for up/down navigation.
struct FlatEntry {
    node_id: u64,
    parent_id: u64, // 0 = root (no parent)
    #[allow(dead_code)]
    depth: u32,
}

impl NavigatorState {
    pub fn new() -> Self {
        Self {
            selected_node: 1, // root node
            collapsed: BTreeSet::new(),
            detail_panel: false,
        }
    }

    /// Handle a navigator input. Returns true if the display needs re-rendering,
    /// or false if nothing changed. Returns None to signal exit.
    pub fn handle_input(&mut self, input: NavInput) -> Option<bool> {
        match input {
            NavInput::Quit => None,
            NavInput::Refresh => Some(true),
            NavInput::ToggleDetail => {
                self.detail_panel = !self.detail_panel;
                Some(true)
            }
            NavInput::ToggleCollapse => {
                let id = self.selected_node;
                // Check if this node has children
                let graph = crate::graph::get();
                if let Some(node) = graph.get_node(id) {
                    let has_children = node.edges.iter().any(|e| {
                        e.label.as_str() == "child" || graph.get_node(e.target).is_some()
                    });
                    if has_children {
                        if self.collapsed.contains(&id) {
                            self.collapsed.remove(&id);
                        } else {
                            self.collapsed.insert(id);
                        }
                        return Some(true);
                    }
                }
                Some(false)
            }
            NavInput::Up | NavInput::Down | NavInput::Left | NavInput::Right => {
                let graph = crate::graph::get();
                let flat = self.build_flat_list(graph, 1, 0, 0);
                if flat.is_empty() {
                    return Some(false);
                }

                let cur_idx = flat.iter().position(|e| e.node_id == self.selected_node);

                match input {
                    NavInput::Up => {
                        if let Some(idx) = cur_idx {
                            if idx > 0 {
                                self.selected_node = flat[idx - 1].node_id;
                                return Some(true);
                            }
                        }
                    }
                    NavInput::Down => {
                        if let Some(idx) = cur_idx {
                            if idx + 1 < flat.len() {
                                self.selected_node = flat[idx + 1].node_id;
                                return Some(true);
                            }
                        }
                    }
                    NavInput::Left => {
                        // Go to parent
                        if let Some(idx) = cur_idx {
                            let parent = flat[idx].parent_id;
                            if parent != 0 {
                                self.selected_node = parent;
                                return Some(true);
                            }
                        }
                    }
                    NavInput::Right => {
                        // Go to first child (expand if collapsed)
                        if let Some(idx) = cur_idx {
                            let id = flat[idx].node_id;
                            // If collapsed, expand first
                            if self.collapsed.contains(&id) {
                                self.collapsed.remove(&id);
                            }
                            // Now find the first child in the flat list
                            // Rebuild flat list after potential uncollapse
                            let flat2 = self.build_flat_list(graph, 1, 0, 0);
                            // Find entries whose parent is our current node
                            for entry in &flat2 {
                                if entry.parent_id == id {
                                    self.selected_node = entry.node_id;
                                    return Some(true);
                                }
                            }
                        }
                    }
                    _ => {}
                }
                Some(false)
            }
        }
    }

    /// Build a depth-first flattened list of visible nodes (respecting collapsed state).
    fn build_flat_list(&self, graph: &crate::graph::Graph, node_id: u64, parent_id: u64, depth: u32) -> Vec<FlatEntry> {
        let mut result = Vec::new();
        let mut visited = Vec::new();
        self.collect_flat(graph, node_id, parent_id, depth, &mut result, &mut visited);
        result
    }

    fn collect_flat(
        &self,
        graph: &crate::graph::Graph,
        node_id: u64,
        parent_id: u64,
        depth: u32,
        result: &mut Vec<FlatEntry>,
        visited: &mut Vec<u64>,
    ) {
        if visited.contains(&node_id) {
            return;
        }
        let node = match graph.get_node(node_id) {
            Some(n) => n,
            None => return,
        };
        visited.push(node_id);

        result.push(FlatEntry {
            node_id,
            parent_id,
            depth,
        });

        // If collapsed, don't recurse into children
        if self.collapsed.contains(&node_id) {
            return;
        }

        // Recurse into children (same order as render: child edges first, then others)
        for edge in &node.edges {
            if edge.label.as_str() == "child" {
                self.collect_flat(graph, edge.target, node_id, depth + 1, result, visited);
            }
        }
        for edge in &node.edges {
            if edge.label.as_str() != "child" {
                self.collect_flat(graph, edge.target, node_id, depth + 1, result, visited);
            }
        }
    }

    /// Ensure the selected node is visible (exists and is in the flat list).
    /// If not, reset to root.
    pub fn ensure_valid_selection(&mut self) {
        let graph = crate::graph::get();
        let flat = self.build_flat_list(graph, 1, 0, 0);
        if !flat.iter().any(|e| e.node_id == self.selected_node) {
            self.selected_node = 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Global navigator state
// ---------------------------------------------------------------------------

static mut NAV_STATE: Option<NavigatorState> = None;

#[allow(static_mut_refs)]
pub fn get() -> &'static NavigatorState {
    unsafe { NAV_STATE.as_ref().expect("navigator not initialized") }
}

#[allow(static_mut_refs)]
pub fn get_mut() -> &'static mut NavigatorState {
    unsafe { NAV_STATE.as_mut().expect("navigator not initialized") }
}

/// Initialize the navigator state (idempotent).
pub fn init() {
    unsafe {
        if NAV_STATE.is_none() {
            NAV_STATE = Some(NavigatorState::new());
        }
    }
}

/// Re-render the navigator view on the framebuffer.
pub fn render_nav() {
    if let Some(fb) = crate::framebuffer::get() {
        let prev = crate::arch::riscv64::interrupts_disable();
        let graph = crate::graph::get();
        let nav = get();
        super::render::render_navigated(fb, graph, nav);
        crate::arch::riscv64::interrupts_restore(prev);
    }
}
