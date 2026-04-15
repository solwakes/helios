/// Framebuffer text console for Helios.
///
/// Provides a retro terminal-style text display on the framebuffer.
/// When active, all print!/println! output goes to both UART and
/// the framebuffer console. Input still comes from UART.

use crate::framebuffer::{Pixel, draw_char, draw_string};

// ---------------------------------------------------------------------------
// Console dimensions and layout
// ---------------------------------------------------------------------------

/// Character cell dimensions (scale 1 font: 8x8 bitmap)
const CHAR_W: u32 = 9;   // 8px char + 1px gap
const CHAR_H: u32 = 10;  // 8px char + 2px line gap

/// Margins and title bar
const MARGIN_X: u32 = 4;
const MARGIN_Y: u32 = 4;
const TITLE_H: u32 = 18;  // title text (8px) + padding + separator

/// Text area origin
const TEXT_X: u32 = MARGIN_X;
const TEXT_Y: u32 = MARGIN_Y + TITLE_H;

/// Grid size: (1024 - 8) / 9 = 112 cols, (768 - 22 - 4) / 10 = 74 rows
const COLS: usize = 112;
const ROWS: usize = 74;

// ---------------------------------------------------------------------------
// Color scheme (retro terminal)
// ---------------------------------------------------------------------------

const BG: Pixel        = Pixel::new(0x0a, 0x0a, 0x1a); // very dark blue-black
const TEXT_C: Pixel    = Pixel::new(0x00, 0xcc, 0x66);  // green, classic terminal
const TITLE_C: Pixel   = Pixel::new(0xf0, 0xa5, 0x00);  // golden amber
const SEP_C: Pixel     = Pixel::new(0x33, 0x33, 0x55);  // border/separator

// ---------------------------------------------------------------------------
// Console state (all static, no heap allocation)
// ---------------------------------------------------------------------------

static mut TEXT_BUF: [[u8; COLS]; ROWS] = [[b' '; COLS]; ROWS];
static mut CURSOR_ROW: usize = 0;
static mut CURSOR_COL: usize = 0;
static mut ACTIVE: bool = false;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Initialize the console module (does NOT activate it).
pub fn init() {
    clear_buf();
}

/// Is the console currently displayed on the framebuffer?
pub fn is_active() -> bool {
    unsafe { ACTIVE }
}

/// Activate or deactivate the framebuffer console.
/// When activated, immediately renders the console.
pub fn set_active(active: bool) {
    unsafe {
        ACTIVE = active;
        if active {
            render();
        }
    }
}

/// Write a single byte to the console (if active).
/// Handles newline, carriage return, backspace, and printable ASCII.
pub fn putc(ch: u8) {
    unsafe {
        if !ACTIVE {
            return;
        }
        match ch {
            b'\n' => {
                CURSOR_COL = 0;
                CURSOR_ROW += 1;
                if CURSOR_ROW >= ROWS {
                    scroll_up();
                }
            }
            b'\r' => {
                CURSOR_COL = 0;
            }
            0x08 => {
                // Backspace — move cursor left
                if CURSOR_COL > 0 {
                    CURSOR_COL -= 1;
                }
            }
            0x20..=0x7E => {
                TEXT_BUF[CURSOR_ROW][CURSOR_COL] = ch;
                render_cell(CURSOR_ROW, CURSOR_COL);
                CURSOR_COL += 1;
                if CURSOR_COL >= COLS {
                    CURSOR_COL = 0;
                    CURSOR_ROW += 1;
                    if CURSOR_ROW >= ROWS {
                        scroll_up();
                    }
                }
            }
            _ => {}
        }
    }
}

/// Write a string to the console, filtering out \r
/// (UART adds \r before \n, but the console handles \n directly).
pub fn write_str(s: &str) {
    if !is_active() {
        return;
    }
    for byte in s.bytes() {
        if byte != b'\r' {
            putc(byte);
        }
    }
}

/// Clear the console buffer and re-render.
pub fn clear() {
    clear_buf();
    if is_active() {
        render();
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Zero out the text buffer and reset cursor.
fn clear_buf() {
    unsafe {
        for row in TEXT_BUF.iter_mut() {
            *row = [b' '; COLS];
        }
        CURSOR_ROW = 0;
        CURSOR_COL = 0;
    }
}

/// Scroll the text buffer up by one line and re-render.
unsafe fn scroll_up() {
    for r in 1..ROWS {
        TEXT_BUF[r - 1] = TEXT_BUF[r];
    }
    TEXT_BUF[ROWS - 1] = [b' '; COLS];
    CURSOR_ROW = ROWS - 1;
    CURSOR_COL = 0;
    render();
}

/// Render a single character cell on the framebuffer.
unsafe fn render_cell(row: usize, col: usize) {
    if let Some(fb) = crate::framebuffer::get() {
        let x = TEXT_X + col as u32 * CHAR_W;
        let y = TEXT_Y + row as u32 * CHAR_H;
        // Clear cell background
        fb.fill_rect(x, y, CHAR_W, CHAR_H, BG);
        // Draw character (only if printable)
        let ch = TEXT_BUF[row][col];
        if ch > 0x20 && ch <= 0x7E {
            draw_char(fb, ch, x, y, 1, TEXT_C);
        }
    }
}

/// Full re-render of the entire console to the framebuffer.
fn render() {
    let fb = match crate::framebuffer::get() {
        Some(fb) => fb,
        None => return,
    };

    let prev = crate::arch::riscv64::interrupts_disable();

    // 1. Clear entire framebuffer
    fb.fill(BG);

    // 2. Title bar: "HELIOS Console"
    draw_string(fb, "HELIOS Console", MARGIN_X, MARGIN_Y, 1, TITLE_C);

    // Separator line below title
    let sep_y = MARGIN_Y + 12;
    fb.draw_hline(MARGIN_X, sep_y, fb.width - 2 * MARGIN_X, SEP_C);

    // 3. Render all characters in the text buffer
    unsafe {
        for row in 0..ROWS {
            for col in 0..COLS {
                let ch = TEXT_BUF[row][col];
                if ch > 0x20 && ch <= 0x7E {
                    let x = TEXT_X + col as u32 * CHAR_W;
                    let y = TEXT_Y + row as u32 * CHAR_H;
                    draw_char(fb, ch, x, y, 1, TEXT_C);
                }
            }
        }
    }

    crate::arch::riscv64::interrupts_restore(prev);
}
