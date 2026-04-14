/// Framebuffer abstraction for Helios.
///
/// Uses ramfb (a simple QEMU display device) to provide a framebuffer.
/// The guest allocates memory, writes a config via fw_cfg, and QEMU displays it.
/// No virtqueues or command protocols needed.

/// A simple RGBA pixel
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Pixel {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Pixel {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }
}

/// Framebuffer info
pub struct Framebuffer {
    pub base: *mut u8,
    pub width: u32,
    pub height: u32,
    pub stride: u32, // bytes per row
    pub bpp: u32,    // bytes per pixel
}

impl Framebuffer {
    /// Fill the entire framebuffer with a solid color
    pub fn fill(&self, pixel: Pixel) {
        if self.base.is_null() {
            return;
        }
        for y in 0..self.height {
            for x in 0..self.width {
                self.put_pixel(x, y, pixel);
            }
        }
    }

    /// Set a single pixel
    pub fn put_pixel(&self, x: u32, y: u32, pixel: Pixel) {
        if self.base.is_null() || x >= self.width || y >= self.height {
            return;
        }
        let offset = (y * self.stride + x * self.bpp) as usize;
        unsafe {
            let ptr = self.base.add(offset);
            // Assume BGRA / XRGB format (common for virtio-gpu)
            ptr.add(0).write_volatile(pixel.b);
            ptr.add(1).write_volatile(pixel.g);
            ptr.add(2).write_volatile(pixel.r);
            ptr.add(3).write_volatile(pixel.a);
        }
    }

    /// Draw a filled rectangle
    pub fn fill_rect(&self, x: u32, y: u32, w: u32, h: u32, pixel: Pixel) {
        for dy in 0..h {
            for dx in 0..w {
                self.put_pixel(x + dx, y + dy, pixel);
            }
        }
    }
}

// =============================================================================
// Bitmap font rendering — a tiny 8x8 font for "HELIOS"
// =============================================================================

/// 8x8 bitmap glyphs for the characters we need: H, E, L, I, O, S, and space
const fn glyph(ch: u8) -> [u8; 8] {
    match ch {
        b'H' => [
            0b10000010,
            0b10000010,
            0b10000010,
            0b11111110,
            0b10000010,
            0b10000010,
            0b10000010,
            0b00000000,
        ],
        b'E' => [
            0b11111110,
            0b10000000,
            0b10000000,
            0b11111100,
            0b10000000,
            0b10000000,
            0b11111110,
            0b00000000,
        ],
        b'L' => [
            0b10000000,
            0b10000000,
            0b10000000,
            0b10000000,
            0b10000000,
            0b10000000,
            0b11111110,
            0b00000000,
        ],
        b'I' => [
            0b11111110,
            0b00010000,
            0b00010000,
            0b00010000,
            0b00010000,
            0b00010000,
            0b11111110,
            0b00000000,
        ],
        b'O' => [
            0b01111100,
            0b10000010,
            0b10000010,
            0b10000010,
            0b10000010,
            0b10000010,
            0b01111100,
            0b00000000,
        ],
        b'S' => [
            0b01111100,
            0b10000010,
            0b10000000,
            0b01111100,
            0b00000010,
            0b10000010,
            0b01111100,
            0b00000000,
        ],
        b' ' => [0; 8],
        _ => [0; 8],
    }
}

/// Draw a character at (x, y) with a given scale factor
pub fn draw_char(fb: &Framebuffer, ch: u8, x: u32, y: u32, scale: u32, color: Pixel) {
    let g = glyph(ch);
    for row in 0..8u32 {
        for col in 0..8u32 {
            if g[row as usize] & (0x80 >> col) != 0 {
                fb.fill_rect(x + col * scale, y + row * scale, scale, scale, color);
            }
        }
    }
}

/// Draw a string at (x, y)
pub fn draw_string(fb: &Framebuffer, s: &str, x: u32, y: u32, scale: u32, color: Pixel) {
    let char_width = 8 * scale + scale; // 8 pixels + 1 pixel gap, scaled
    for (i, ch) in s.bytes().enumerate() {
        draw_char(fb, ch, x + (i as u32) * char_width, y, scale, color);
    }
}

// =============================================================================
// ramfb initialisation + splash screen
// =============================================================================

pub fn init() {
    crate::println!("[fb] Initialising ramfb framebuffer...");

    let info = match crate::ramfb::init() {
        Some(info) => info,
        None => {
            crate::println!("[fb] No ramfb available — UART only");
            return;
        }
    };

    let fb = Framebuffer {
        base: info.fb_ptr,
        width: info.width,
        height: info.height,
        stride: info.width * 4,
        bpp: 4,
    };

    // Dark indigo background (#1a1a2e)
    let bg = Pixel::new(0x1a, 0x1a, 0x2e);
    fb.fill(bg);

    // Golden / amber text (#f0a500)
    let gold = Pixel::new(0xf0, 0xa5, 0x00);
    let text = "HELIOS";
    let scale: u32 = 8; // 8×8 font × 8 = 64px tall
    let char_w = 8 * scale + scale; // pixel width per glyph (with gap)
    let text_w = text.len() as u32 * char_w - scale; // subtract trailing gap
    let text_h = 8 * scale;
    let x = (fb.width.saturating_sub(text_w)) / 2;
    let y = (fb.height.saturating_sub(text_h)) / 2;
    draw_string(&fb, text, x, y, scale, gold);

    // ramfb is a direct framebuffer — no flush needed, pixels are live in RAM
    crate::println!("[fb] Splash rendered. Framebuffer ready.");
}
