/// Framebuffer abstraction for Helios.
///
/// For M1, we implement a simple VirtIO GPU framebuffer using the virtio-gpu
/// device on QEMU's virt machine. If this proves too complex, we fall back
/// to UART-only output and defer graphics to M2.
///
/// For now, this module provides a software framebuffer that can be used
/// once we have a display device initialized.

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
// VirtIO GPU initialization (M1 attempt — may be deferred to M2)
// =============================================================================

/// For M1, VirtIO GPU setup is complex (requires virtqueue setup, resource
/// creation, scanout attachment, etc.). We'll document the approach here
/// and implement it properly in M2.
///
/// What's needed for M2:
/// 1. VirtIO device discovery via MMIO (base at 0x10008000 on virt machine)
/// 2. Virtqueue initialization (descriptor table, available ring, used ring)
/// 3. GPU resource creation (VIRTIO_GPU_CMD_RESOURCE_CREATE_2D)
/// 4. Backing storage attachment (VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING)
/// 5. Scanout setup (VIRTIO_GPU_CMD_SET_SCANOUT)
/// 6. Transfer & flush (VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D, VIRTIO_GPU_CMD_RESOURCE_FLUSH)

pub fn init() {
    // VirtIO GPU initialization deferred to M2.
    // For M1, we output via UART only.
    crate::println!("[fb] Framebuffer initialization deferred to M2 (VirtIO GPU)");
}
