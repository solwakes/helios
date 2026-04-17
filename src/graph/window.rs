/// Window Manager — graph nodes as floating windows.
///
/// Philosophically: windows ARE nodes, z-order IS a graph property,
/// focus IS the graph's current pointer.
///
/// Window state is stored as a side-table keyed by node_id, so the
/// on-disk graph persistence format is unchanged. Windowed state is
/// ephemeral (in-RAM only) but easy to snapshot later if needed.
///
/// Rendering path:
///   graph navigator (tree) -> windows (sorted by z) -> cursor overlay
///
/// Mouse interaction (in nav mode):
///   1. If a drag is in progress, update position and exit.
///   2. Else, hit-test windows (topmost z first).
///      - Click on title bar: focus + begin drag.
///      - Click on body:      focus (bring to front).
///   3. If no window hit, fall through to graph navigator hit testing.
///
/// A new shell command `window <id>` toggles windowed mode on a node.

use alloc::vec::Vec;
use alloc::format;
use alloc::string::String;

use crate::framebuffer::{Framebuffer, Pixel, draw_string, draw_char};

// ---------------------------------------------------------------------------
// Window metadata
// ---------------------------------------------------------------------------

/// Per-window state. Windowed nodes have one entry in the side-table.
#[derive(Clone)]
pub struct WindowState {
    pub node_id: u64,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub z: u32,
}

/// Transient drag state (separate from WindowState so we don't have to
/// mutate every window on cursor moves that aren't drags).
#[derive(Clone, Copy)]
struct DragState {
    node_id: u64,
    /// Offset from cursor to window top-left, frozen at drag start.
    dx: i32,
    dy: i32,
}

/// Global window manager state.
pub struct WindowManager {
    pub windows: Vec<WindowState>,
    pub focused: Option<u64>,
    pub next_z: u32,
    drag: Option<DragState>,
}

impl WindowManager {
    pub const fn new() -> Self {
        Self {
            windows: Vec::new(),
            focused: None,
            next_z: 1,
            drag: None,
        }
    }

    /// Is this node currently windowed?
    pub fn is_windowed(&self, id: u64) -> bool {
        self.windows.iter().any(|w| w.node_id == id)
    }

    /// Find the index of a window by node_id.
    fn find_index(&self, id: u64) -> Option<usize> {
        self.windows.iter().position(|w| w.node_id == id)
    }

    /// Open a new window for `node_id` at (x,y) with (w,h), returning the
    /// assigned z-order. If a window for this node already exists, returns
    /// None (caller should use the existing one).
    pub fn open(&mut self, node_id: u64, x: i32, y: i32, w: u32, h: u32) -> bool {
        if self.is_windowed(node_id) {
            return false;
        }
        let z = self.next_z;
        self.next_z += 1;
        self.windows.push(WindowState {
            node_id,
            x,
            y,
            w,
            h,
            z,
        });
        self.focused = Some(node_id);
        true
    }

    /// Close the window for this node (if any). Returns true if it was closed.
    pub fn close(&mut self, node_id: u64) -> bool {
        match self.find_index(node_id) {
            Some(idx) => {
                self.windows.remove(idx);
                if self.focused == Some(node_id) {
                    // Give focus to the topmost remaining window, if any.
                    self.focused = self
                        .windows
                        .iter()
                        .max_by_key(|w| w.z)
                        .map(|w| w.node_id);
                }
                if self.drag.map(|d| d.node_id) == Some(node_id) {
                    self.drag = None;
                }
                true
            }
            None => false,
        }
    }

    /// Focus (bring to front) this window, giving it the highest z-order.
    pub fn focus(&mut self, node_id: u64) {
        if self.find_index(node_id).is_none() {
            return;
        }
        let z = self.next_z;
        self.next_z += 1;
        if let Some(idx) = self.find_index(node_id) {
            self.windows[idx].z = z;
        }
        self.focused = Some(node_id);
    }

    /// Begin a drag on this window — anchored at the current cursor.
    pub fn begin_drag(&mut self, node_id: u64, cx: i32, cy: i32) {
        if let Some(idx) = self.find_index(node_id) {
            let w = &self.windows[idx];
            self.drag = Some(DragState {
                node_id,
                dx: w.x - cx,
                dy: w.y - cy,
            });
        }
    }

    /// End any in-progress drag.
    pub fn end_drag(&mut self) {
        self.drag = None;
    }

    /// Is a drag currently in progress?
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Update position from current cursor. Returns true if a redraw is needed.
    pub fn update_drag(&mut self, cx: i32, cy: i32) -> bool {
        let drag = match self.drag {
            Some(d) => d,
            None => return false,
        };
        let idx = match self.find_index(drag.node_id) {
            Some(i) => i,
            None => return false,
        };
        let new_x = cx + drag.dx;
        let new_y = cy + drag.dy;
        let w = &mut self.windows[idx];
        if w.x == new_x && w.y == new_y {
            return false;
        }
        w.x = new_x;
        w.y = new_y;
        true
    }

    /// Hit-test: which window (if any) contains (cx, cy)?
    /// Returns (node_id, on_title_bar) for the topmost window.
    pub fn hit_test(&self, cx: i32, cy: i32) -> Option<(u64, bool)> {
        // Iterate in reverse z-order (highest z first).
        let mut indices: Vec<usize> = (0..self.windows.len()).collect();
        indices.sort_by_key(|&i| core::cmp::Reverse(self.windows[i].z));
        for i in indices {
            let w = &self.windows[i];
            if cx >= w.x
                && cx < w.x + w.w as i32
                && cy >= w.y
                && cy < w.y + w.h as i32
            {
                let on_title = cy < w.y + TITLE_H as i32;
                // Close button hit zone (rightmost TITLE_H pixels of the title bar).
                let close_left = w.x + w.w as i32 - TITLE_H as i32;
                if on_title && cx >= close_left {
                    return Some((w.node_id, true)); // title click handling distinguishes close
                }
                return Some((w.node_id, on_title));
            }
        }
        None
    }

    /// Was the click on the close button specifically?
    pub fn hit_close(&self, cx: i32, cy: i32) -> Option<u64> {
        let mut indices: Vec<usize> = (0..self.windows.len()).collect();
        indices.sort_by_key(|&i| core::cmp::Reverse(self.windows[i].z));
        for i in indices {
            let w = &self.windows[i];
            if cx >= w.x
                && cx < w.x + w.w as i32
                && cy >= w.y
                && cy < w.y + w.h as i32
            {
                let on_title = cy < w.y + TITLE_H as i32;
                let close_left = w.x + w.w as i32 - TITLE_H as i32;
                if on_title && cx >= close_left {
                    return Some(w.node_id);
                }
                return None;
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Global WM instance
// ---------------------------------------------------------------------------

static mut WM: WindowManager = WindowManager::new();

#[allow(static_mut_refs)]
pub fn get() -> &'static WindowManager {
    unsafe { &WM }
}

#[allow(static_mut_refs)]
pub fn get_mut() -> &'static mut WindowManager {
    unsafe { &mut WM }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

const TITLE_H: u32 = 22;

// Color palette — harmonized with graph/render.rs
const WIN_BG: Pixel             = Pixel::new(0x1b, 0x1e, 0x3a); // dark indigo
const WIN_BORDER: Pixel         = Pixel::new(0x44, 0x55, 0x77);
const WIN_BORDER_FOCUSED: Pixel = Pixel::new(0xf0, 0xa5, 0x00); // amber
const TITLE_BG_FOCUSED: Pixel   = Pixel::new(0xf0, 0xa5, 0x00); // golden amber
const TITLE_BG_UNFOCUSED: Pixel = Pixel::new(0x3a, 0x3f, 0x60); // muted indigo
const TITLE_TEXT_FOCUSED: Pixel = Pixel::new(0x1a, 0x0e, 0x00); // near-black on amber
const TITLE_TEXT_UNFOCUSED: Pixel = Pixel::new(0xcc, 0xcc, 0xdd);
const WIN_TEXT: Pixel           = Pixel::new(0x00, 0xff, 0xaa); // terminal green
const WIN_LABEL: Pixel          = Pixel::new(0x88, 0x99, 0xbb);
const CLOSE_X: Pixel            = Pixel::new(0xff, 0x66, 0x66);

/// Small glyph dimensions (scale 1).
const CHAR_W_S: u32 = 9;
const CHAR_H_S: u32 = 10;

/// Title-bar character dimensions (scale 1).
const TITLE_CHAR_W: u32 = 9;

/// Clip a rect to the framebuffer, returning visible (x, y, w, h) or None.
fn clip_rect(fb: &Framebuffer, x: i32, y: i32, w: u32, h: u32) -> Option<(u32, u32, u32, u32)> {
    let x_end = x.saturating_add(w as i32);
    let y_end = y.saturating_add(h as i32);
    if x_end <= 0 || y_end <= 0 || x >= fb.width as i32 || y >= fb.height as i32 {
        return None;
    }
    let cx = x.max(0) as u32;
    let cy = y.max(0) as u32;
    let cw = (x_end.min(fb.width as i32) - cx as i32) as u32;
    let ch = (y_end.min(fb.height as i32) - cy as i32) as u32;
    if cw == 0 || ch == 0 {
        return None;
    }
    Some((cx, cy, cw, ch))
}

/// Draw all windows, sorted by z-order (ascending — lowest first, so
/// the focused/top-most window draws last and appears on top).
pub fn render_all(fb: &Framebuffer) {
    let wm = get();
    if wm.windows.is_empty() {
        return;
    }

    // Sort indices by z-order ascending.
    let mut indices: Vec<usize> = (0..wm.windows.len()).collect();
    indices.sort_by_key(|&i| wm.windows[i].z);

    let graph = crate::graph::get();
    let focused = wm.focused;
    for i in indices {
        let w = &wm.windows[i];
        let is_focused = focused == Some(w.node_id);
        draw_window(fb, graph, w, is_focused);
    }
}

fn draw_window(fb: &Framebuffer, graph: &crate::graph::Graph, w: &WindowState, focused: bool) {
    // Background (including title area): fill the whole window rect.
    if let Some((cx, cy, cw, ch)) = clip_rect(fb, w.x, w.y, w.w, w.h) {
        fb.fill_rect(cx, cy, cw, ch, WIN_BG);
    }

    // Title bar.
    let title_bg = if focused { TITLE_BG_FOCUSED } else { TITLE_BG_UNFOCUSED };
    if let Some((cx, cy, cw, ch)) = clip_rect(fb, w.x, w.y, w.w, TITLE_H) {
        fb.fill_rect(cx, cy, cw, ch, title_bg);
    }

    // Title text (node id + name).
    let title = match graph.get_node(w.node_id) {
        Some(node) => format!("#{} {}", node.id, node.name),
        None => format!("#{} (gone)", w.node_id),
    };
    let title_color = if focused { TITLE_TEXT_FOCUSED } else { TITLE_TEXT_UNFOCUSED };
    let title_x = w.x + 6;
    let title_y = w.y + (TITLE_H as i32 - 8) / 2;
    // Clip text by hand — draw_string clips per-pixel but only if x/y are unsigned.
    // We only draw the title if origin is within the framebuffer.
    if title_x >= 0 && title_y >= 0
        && (title_x as u32) < fb.width
        && (title_y as u32) < fb.height
    {
        // Truncate to available width (minus space for close button).
        let avail = w.w.saturating_sub(6 + TITLE_H + 4);
        let max_chars = (avail / TITLE_CHAR_W) as usize;
        let display: String = if title.len() > max_chars && max_chars > 1 {
            let end = max_chars.saturating_sub(1);
            let mut s = String::new();
            for (i, ch) in title.chars().enumerate() {
                if i >= end { break; }
                s.push(ch);
            }
            s.push('~');
            s
        } else {
            title
        };
        draw_string(fb, &display, title_x as u32, title_y as u32, 1, title_color);
    }

    // Close button (×) at right side of title bar.
    let cb_x = w.x + w.w as i32 - TITLE_H as i32;
    let cb_y = w.y;
    // Draw X on a slightly darker/bolder square.
    let x_glyph_x = cb_x + (TITLE_H as i32 - 8) / 2;
    let x_glyph_y = cb_y + (TITLE_H as i32 - 8) / 2;
    if x_glyph_x >= 0 && x_glyph_y >= 0
        && (x_glyph_x as u32) < fb.width
        && (x_glyph_y as u32) < fb.height
    {
        draw_char(fb, b'x', x_glyph_x as u32, x_glyph_y as u32, 1, CLOSE_X);
    }

    // Outer border — double-thick when focused for emphasis.
    let border_color = if focused { WIN_BORDER_FOCUSED } else { WIN_BORDER };
    draw_rect_outline_clipped(fb, w.x, w.y, w.w, w.h, border_color);
    if focused {
        draw_rect_outline_clipped(fb, w.x + 1, w.y + 1, w.w.saturating_sub(2), w.h.saturating_sub(2), border_color);
    }
    // Separator under title bar.
    draw_hline_s(fb, w.x, w.x + w.w as i32 - 1, w.y + TITLE_H as i32, border_color);

    // Body content: render node text with word wrap.
    draw_window_body(fb, graph, w);
}

fn draw_window_body(fb: &Framebuffer, graph: &crate::graph::Graph, w: &WindowState) {
    let pad: u32 = 6;
    let body_x = w.x + pad as i32;
    let body_y = w.y + TITLE_H as i32 + pad as i32;
    let body_w = w.w.saturating_sub(2 * pad);
    let body_h = w.h.saturating_sub(TITLE_H + 2 * pad);

    if body_w < CHAR_W_S || body_h < CHAR_H_S {
        return;
    }

    let max_cols = (body_w / CHAR_W_S) as usize;
    let max_rows = (body_h / CHAR_H_S) as usize;

    let node = match graph.get_node(w.node_id) {
        Some(n) => n,
        None => {
            // Node deleted — show a placeholder.
            if body_x >= 0 && body_y >= 0
                && (body_x as u32) < fb.width
                && (body_y as u32) < fb.height
            {
                draw_string(fb, "(node removed)", body_x as u32, body_y as u32, 1, WIN_LABEL);
            }
            return;
        }
    };

    // Header line: type tag.
    let type_label = format!("type: {}", node.type_tag);
    if body_x >= 0 && body_y >= 0
        && (body_x as u32) < fb.width
        && (body_y as u32) < fb.height
    {
        draw_string(fb, &type_label, body_x as u32, body_y as u32, 1, WIN_LABEL);
    }

    let content = node.display_content(graph);

    let start_row: usize = 2; // leave a gap below the type label
    let mut row = start_row;

    for line in content.split('\n') {
        if row >= max_rows {
            break;
        }

        let mut remaining = line;
        loop {
            if row >= max_rows {
                break;
            }
            let chunk = if remaining.len() > max_cols {
                &remaining[..max_cols]
            } else {
                remaining
            };
            let y = body_y + (row as i32) * CHAR_H_S as i32;
            if body_x >= 0 && y >= 0
                && (body_x as u32) < fb.width
                && (y as u32) < fb.height
            {
                draw_string(fb, chunk, body_x as u32, y as u32, 1, WIN_TEXT);
            }
            row += 1;
            if remaining.len() > max_cols {
                remaining = &remaining[max_cols..];
            } else {
                break;
            }
        }
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

fn draw_vline_s(fb: &Framebuffer, x: i32, y0: i32, y1: i32, color: Pixel) {
    if x < 0 || x >= fb.width as i32 { return; }
    let (ya, yb) = if y0 <= y1 { (y0, y1) } else { (y1, y0) };
    let ya = ya.max(0) as u32;
    let yb = yb.min(fb.height as i32 - 1) as u32;
    if ya <= yb {
        fb.draw_vline(x as u32, ya, yb - ya + 1, color);
    }
}

fn draw_rect_outline_clipped(fb: &Framebuffer, x: i32, y: i32, w: u32, h: u32, color: Pixel) {
    if w < 1 || h < 1 {
        return;
    }
    draw_hline_s(fb, x, x + w as i32 - 1, y, color);
    draw_hline_s(fb, x, x + w as i32 - 1, y + h as i32 - 1, color);
    draw_vline_s(fb, x, y, y + h as i32 - 1, color);
    draw_vline_s(fb, x + w as i32 - 1, y, y + h as i32 - 1, color);
}

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

/// Default window size. Different node content will wrap inside these bounds.
pub const DEFAULT_W: u32 = 280;
pub const DEFAULT_H: u32 = 180;

/// Spawn a window for a node at a position. If already windowed, just focus it.
pub fn toggle_window(node_id: u64, x: i32, y: i32) -> bool {
    let wm = get_mut();
    if wm.is_windowed(node_id) {
        wm.close(node_id);
        false
    } else {
        wm.open(node_id, x, y, DEFAULT_W, DEFAULT_H);
        true
    }
}

/// Spawn a set of demo windows at boot (cpu / memory / timer).
/// Positions are chosen so they don't obscure the title bar.
pub fn boot_demo_windows() {
    let wm = get_mut();
    // IDs come from graph::init::bootstrap()
    //   #6 memory, #7 timer, #8 cpu
    if wm.is_windowed(6) || wm.is_windowed(7) || wm.is_windowed(8) {
        return;
    }
    wm.open(8, 40, 120, DEFAULT_W, DEFAULT_H);          // cpu, left
    wm.open(6, 360, 520, DEFAULT_W + 40, DEFAULT_H);    // memory, bottom-center
    wm.open(7, 700, 120, DEFAULT_W, DEFAULT_H);         // timer, right
    // Focus ends up on the last opened — explicitly set to cpu for a tidy
    // layout (topmost == cpu at the left).
    wm.focus(8);
}
