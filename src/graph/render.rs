/// Graph visualization renderer for the ramfb framebuffer.
///
/// Renders graph nodes as cards in a 2-column grid layout with edges listed
/// inside each card. Uses the Helios solar color scheme.

use crate::framebuffer::{Framebuffer, Pixel, draw_string};
use super::Graph;
use alloc::format;

// ---------------------------------------------------------------------------
// Color scheme — solar theme
// ---------------------------------------------------------------------------

const BG: Pixel           = Pixel::new(0x1a, 0x1a, 0x2e); // dark indigo
const TITLE_COLOR: Pixel  = Pixel::new(0xf0, 0xa5, 0x00); // golden amber
const CARD_BG: Pixel      = Pixel::new(0x25, 0x25, 0x47); // lighter indigo
const CARD_BORDER: Pixel  = Pixel::new(0x44, 0x44, 0x66); // subtle border
const NODE_NAME: Pixel    = Pixel::new(0xff, 0xff, 0xff); // white
const NODE_TYPE: Pixel    = Pixel::new(0x88, 0x88, 0x99); // dim
const NODE_ID: Pixel      = Pixel::new(0xf0, 0xa5, 0x00); // gold
const EDGE_COLOR: Pixel   = Pixel::new(0x66, 0xcc, 0xff); // light blue
const LINE_COLOR: Pixel   = Pixel::new(0x44, 0x44, 0x66); // subtle lines

// ---------------------------------------------------------------------------
// Layout constants (all in pixels)
// ---------------------------------------------------------------------------

const SCALE: u32 = 2;                    // text scale (16px tall)
const CHAR_W: u32 = 8 * SCALE + SCALE;   // 18px per char (with gap)
const CHAR_H: u32 = 8 * SCALE;           // 16px tall
const LINE_H: u32 = CHAR_H + 4;          // 20px line spacing

const MARGIN_X: u32 = 30;                // left/right margin
const MARGIN_Y: u32 = 10;                // top margin
const TITLE_Y: u32 = MARGIN_Y;
const CARDS_START_Y: u32 = TITLE_Y + LINE_H + 20; // below title bar

const CARD_PAD: u32 = 10;                // padding inside card
const CARD_GAP_X: u32 = 20;              // horizontal gap between cards
const CARD_GAP_Y: u32 = 16;              // vertical gap between cards
const NUM_COLS: u32 = 2;

/// Maximum characters we can fit in a card title (truncate to avoid overflow)
const MAX_LABEL_LEN: usize = 24;

// ---------------------------------------------------------------------------
// Public render entry point
// ---------------------------------------------------------------------------

/// Render the graph onto the framebuffer.
pub fn render(fb: &Framebuffer, graph: &Graph) {
    // 1. Clear background
    fb.fill(BG);

    // 2. Title bar
    draw_string(fb, "HELIOS - Graph Memory", MARGIN_X, TITLE_Y, SCALE, TITLE_COLOR);

    // Separator line below title
    let sep_y = TITLE_Y + LINE_H + 8;
    fb.draw_hline(MARGIN_X, sep_y, fb.width - 2 * MARGIN_X, LINE_COLOR);

    // 3. Compute card width based on framebuffer
    let total_w = fb.width - 2 * MARGIN_X;
    let card_w = (total_w - (NUM_COLS - 1) * CARD_GAP_X) / NUM_COLS;

    // 4. Render each node as a card
    let mut col = 0u32;
    let mut row_y = CARDS_START_Y;
    let mut max_card_h_in_row: u32 = 0;

    for node in graph.nodes.values() {
        // Calculate card height: header (id+name) + type line + edges
        let num_edges = node.edges.len() as u32;
        let card_lines = 2 + num_edges; // name line + type line + edges
        let card_h = 2 * CARD_PAD + card_lines * LINE_H;

        // Check if we overflow the screen vertically
        if row_y + card_h > fb.height - 10 {
            break; // stop rendering if we run out of space
        }

        let card_x = MARGIN_X + col * (card_w + CARD_GAP_X);
        let card_y = row_y;

        // Draw card background
        fb.fill_rect(card_x, card_y, card_w, card_h, CARD_BG);

        // Draw card border
        fb.draw_rect_outline(card_x, card_y, card_w, card_h, CARD_BORDER);

        // Inner content position
        let cx = card_x + CARD_PAD;
        let mut cy = card_y + CARD_PAD;

        // Line 1: "#<id> <name>" — id in gold, name in white
        let id_str = format!("#{}", node.id);
        draw_string(fb, &id_str, cx, cy, SCALE, NODE_ID);

        let name_x = cx + (id_str.len() as u32 + 1) * CHAR_W;
        let max_name_chars = ((card_w - 2 * CARD_PAD) / CHAR_W) as usize;
        let name_display = if node.name.len() > max_name_chars.saturating_sub(id_str.len() + 1) {
            &node.name[..max_name_chars.saturating_sub(id_str.len() + 1).min(node.name.len())]
        } else {
            &node.name
        };
        draw_string(fb, name_display, name_x, cy, SCALE, NODE_NAME);
        cy += LINE_H;

        // Line 2: "(type)" in dim
        let type_str = format!("({})", node.type_tag);
        draw_string(fb, &type_str, cx, cy, SCALE, NODE_TYPE);
        cy += LINE_H;

        // Edges
        for edge in &node.edges {
            let target_name = graph
                .get_node(edge.target)
                .map(|n| n.name.as_str())
                .unwrap_or("???");

            let edge_str = format!("-> #{} {} ({})", edge.target, target_name, edge.label);
            let edge_display = if edge_str.len() > MAX_LABEL_LEN {
                &edge_str[..MAX_LABEL_LEN]
            } else {
                &edge_str
            };
            draw_string(fb, edge_display, cx, cy, SCALE, EDGE_COLOR);
            cy += LINE_H;
        }

        // Track max card height for this row
        if card_h > max_card_h_in_row {
            max_card_h_in_row = card_h;
        }

        // Advance to next column/row
        col += 1;
        if col >= NUM_COLS {
            col = 0;
            row_y += max_card_h_in_row + CARD_GAP_Y;
            max_card_h_in_row = 0;
        }
    }

    // 5. Node count summary at bottom
    let summary = format!("{} nodes, {} edges", graph.node_count(), graph.edge_count());
    let summary_y = fb.height - MARGIN_Y - CHAR_H;
    draw_string(fb, &summary, MARGIN_X, summary_y, SCALE, NODE_TYPE);
}
