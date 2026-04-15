/// DOOM port for Helios — platform integration layer.
///
/// Implements the doomgeneric platform interface (DG_* functions) and
/// bridges Doom's C runtime needs to Helios kernel services.

use core::alloc::Layout;

use crate::arch::riscv64 as arch;
use crate::framebuffer;
use crate::uart;

// ─── WAD data embedded in the kernel binary ───────────────────────────────

static DOOM1_WAD: &[u8] = include_bytes!("../doom1.wad");

// ─── Globals exported to C ────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn helios_get_wad_data() -> *const u8 {
    DOOM1_WAD.as_ptr()
}

#[no_mangle]
pub extern "C" fn helios_get_wad_size() -> usize {
    DOOM1_WAD.len()
}

// ─── Doom mode flag ──────────────────────────────────────────────────────

static mut DOOM_MODE: bool = false;

pub fn is_doom_mode() -> bool {
    unsafe { DOOM_MODE }
}

// ─── Key event ring buffer ───────────────────────────────────────────────

const KEY_QUEUE_SIZE: usize = 64;

struct KeyEvent {
    pressed: i32,
    key: u8,
}

static mut KEY_QUEUE: [KeyEvent; KEY_QUEUE_SIZE] = {
    const EMPTY: KeyEvent = KeyEvent { pressed: 0, key: 0 };
    [EMPTY; KEY_QUEUE_SIZE]
};
static mut KEY_QUEUE_HEAD: usize = 0;
static mut KEY_QUEUE_TAIL: usize = 0;

pub fn push_key_event(pressed: bool, doom_key: u8) {
    unsafe {
        let next = (KEY_QUEUE_HEAD + 1) % KEY_QUEUE_SIZE;
        if next == KEY_QUEUE_TAIL {
            return; // queue full, drop event
        }
        KEY_QUEUE[KEY_QUEUE_HEAD] = KeyEvent {
            pressed: if pressed { 1 } else { 0 },
            key: doom_key,
        };
        KEY_QUEUE_HEAD = next;
    }
}

fn pop_key_event() -> Option<(i32, u8)> {
    unsafe {
        if KEY_QUEUE_HEAD == KEY_QUEUE_TAIL {
            return None;
        }
        let ev = &KEY_QUEUE[KEY_QUEUE_TAIL];
        let result = (ev.pressed, ev.key);
        KEY_QUEUE_TAIL = (KEY_QUEUE_TAIL + 1) % KEY_QUEUE_SIZE;
        Some(result)
    }
}

// ─── Evdev keycode to Doom key mapping ───────────────────────────────────
// Linux evdev keycodes → Doom key constants (from doomkeys.h)

// Doom key constants
const DOOM_KEY_RIGHTARROW: u8 = 0xae;
const DOOM_KEY_LEFTARROW: u8 = 0xac;
const DOOM_KEY_UPARROW: u8 = 0xad;
const DOOM_KEY_DOWNARROW: u8 = 0xaf;
const DOOM_KEY_ESCAPE: u8 = 27;
const DOOM_KEY_ENTER: u8 = 13;
const DOOM_KEY_TAB: u8 = 9;
const DOOM_KEY_FIRE: u8 = 0xa3;       // Ctrl
const DOOM_KEY_USE: u8 = 0xa2;        // Space mapped to USE? No, Space = ' '. Alt = USE.
const DOOM_KEY_RSHIFT: u8 = 0x80 + 0x36;
const DOOM_KEY_BACKSPACE: u8 = 0x7f;
const DOOM_KEY_F1: u8 = 0x80 + 0x3b;
const DOOM_KEY_F2: u8 = 0x80 + 0x3c;
const DOOM_KEY_F3: u8 = 0x80 + 0x3d;
const DOOM_KEY_F4: u8 = 0x80 + 0x3e;
const DOOM_KEY_F5: u8 = 0x80 + 0x3f;
const DOOM_KEY_F6: u8 = 0x80 + 0x40;
const DOOM_KEY_F7: u8 = 0x80 + 0x41;
const DOOM_KEY_F8: u8 = 0x80 + 0x42;
const DOOM_KEY_F9: u8 = 0x80 + 0x43;
const DOOM_KEY_F10: u8 = 0x80 + 0x44;
const DOOM_KEY_F11: u8 = 0x80 + 0x57;
const DOOM_KEY_F12: u8 = 0x80 + 0x58;
const DOOM_KEY_CAPSLOCK: u8 = 0x80 + 0x3a;
const DOOM_KEY_RALT: u8 = 0x80 + 0x38;

/// Map Linux evdev keycode to Doom key constant.
/// Returns 0 for unmapped keys.
pub fn evdev_to_doom(code: u16) -> u8 {
    match code {
        1 => DOOM_KEY_ESCAPE,       // KEY_ESC
        2 => b'1',                  // KEY_1
        3 => b'2',
        4 => b'3',
        5 => b'4',
        6 => b'5',
        7 => b'6',
        8 => b'7',
        9 => b'8',
        10 => b'9',
        11 => b'0',
        12 => b'-',                 // KEY_MINUS
        13 => b'=',                 // KEY_EQUAL
        14 => DOOM_KEY_BACKSPACE,   // KEY_BACKSPACE
        15 => DOOM_KEY_TAB,         // KEY_TAB
        16 => b'q',
        17 => b'w',
        18 => b'e',
        19 => b'r',
        20 => b't',
        21 => b'y',
        22 => b'u',
        23 => b'i',
        24 => b'o',
        25 => b'p',
        26 => b'[',
        27 => b']',
        28 => DOOM_KEY_ENTER,       // KEY_ENTER
        29 => DOOM_KEY_FIRE,        // KEY_LEFTCTRL → fire
        30 => b'a',
        31 => b's',
        32 => b'd',
        33 => b'f',
        34 => b'g',
        35 => b'h',
        36 => b'j',
        37 => b'k',
        38 => b'l',
        39 => b';',
        40 => b'\'',
        41 => b'`',
        42 => DOOM_KEY_RSHIFT,      // KEY_LEFTSHIFT
        43 => b'\\',
        44 => b'z',
        45 => b'x',
        46 => b'c',
        47 => b'v',
        48 => b'b',
        49 => b'n',
        50 => b'm',
        51 => b',',
        52 => b'.',
        53 => b'/',
        54 => DOOM_KEY_RSHIFT,      // KEY_RIGHTSHIFT
        56 => DOOM_KEY_RALT,        // KEY_LEFTALT → use
        57 => b' ',                 // KEY_SPACE (use key in default config is space)
        58 => DOOM_KEY_CAPSLOCK,    // KEY_CAPSLOCK
        59 => DOOM_KEY_F1,
        60 => DOOM_KEY_F2,
        61 => DOOM_KEY_F3,
        62 => DOOM_KEY_F4,
        63 => DOOM_KEY_F5,
        64 => DOOM_KEY_F6,
        65 => DOOM_KEY_F7,
        66 => DOOM_KEY_F8,
        67 => DOOM_KEY_F9,
        68 => DOOM_KEY_F10,
        87 => DOOM_KEY_F11,
        88 => DOOM_KEY_F12,
        97 => DOOM_KEY_FIRE,        // KEY_RIGHTCTRL → fire
        100 => DOOM_KEY_RALT,       // KEY_RIGHTALT
        103 => DOOM_KEY_UPARROW,    // KEY_UP
        105 => DOOM_KEY_LEFTARROW,  // KEY_LEFT
        106 => DOOM_KEY_RIGHTARROW, // KEY_RIGHT
        108 => DOOM_KEY_DOWNARROW,  // KEY_DOWN
        _ => 0,
    }
}

// ─── Rust-side helpers exported to C ─────────────────────────────────────

#[no_mangle]
pub extern "C" fn helios_alloc(size: usize) -> *mut u8 {
    let layout = match Layout::from_size_align(size, 16) {
        Ok(l) => l,
        Err(_) => return core::ptr::null_mut(),
    };
    unsafe { alloc::alloc::alloc(layout) }
}

#[no_mangle]
pub extern "C" fn helios_dealloc(ptr: *mut u8, size: usize) {
    if ptr.is_null() {
        return;
    }
    let layout = match Layout::from_size_align(size, 16) {
        Ok(l) => l,
        Err(_) => return,
    };
    unsafe { alloc::alloc::dealloc(ptr, layout) }
}

#[no_mangle]
pub extern "C" fn helios_uart_putc(c: u8) {
    uart::putc(c);
}

// ─── DG_* platform functions ─────────────────────────────────────────────

extern "C" {
    static mut DG_ScreenBuffer: *mut u32;
}

#[no_mangle]
pub extern "C" fn DG_Init() {
    // Clear key queue
    unsafe {
        KEY_QUEUE_HEAD = 0;
        KEY_QUEUE_TAIL = 0;
    }
    crate::println!("[doom] DG_Init called");
}

#[no_mangle]
pub extern "C" fn DG_DrawFrame() {
    let fb = match framebuffer::get() {
        Some(fb) => fb,
        None => return,
    };

    let screen_buf = unsafe { DG_ScreenBuffer };
    if screen_buf.is_null() {
        return;
    }

    let doom_w: u32 = 320;
    let doom_h: u32 = 200;

    // Scale factor: try 2x, fall back to 1x
    let scale = if fb.width >= doom_w * 2 && fb.height >= doom_h * 2 { 2u32 } else { 1u32 };
    let scaled_w = doom_w * scale;
    let scaled_h = doom_h * scale;

    // Center on screen
    let off_x = (fb.width.saturating_sub(scaled_w)) / 2;
    let off_y = (fb.height.saturating_sub(scaled_h)) / 2;

    // Blit pixels
    // Doom pixel format: XRGB8888 = 0x00RRGGBB
    // Our framebuffer: BGRA byte order (B, G, R, A)
    // So for each doom pixel, write B, G, R, 0xFF
    for y in 0..doom_h {
        for x in 0..doom_w {
            let pixel = unsafe { *screen_buf.add((y * doom_w + x) as usize) };
            let r = ((pixel >> 16) & 0xFF) as u8;
            let g = ((pixel >> 8) & 0xFF) as u8;
            let b = (pixel & 0xFF) as u8;

            if scale == 2 {
                let fb_x = off_x + x * 2;
                let fb_y = off_y + y * 2;
                // Write 2x2 block directly for speed
                for dy in 0..2u32 {
                    let row_offset = ((fb_y + dy) * fb.stride + fb_x * fb.bpp) as usize;
                    unsafe {
                        let ptr = fb.base.add(row_offset);
                        // Pixel 0
                        ptr.add(0).write_volatile(b);
                        ptr.add(1).write_volatile(g);
                        ptr.add(2).write_volatile(r);
                        ptr.add(3).write_volatile(0xFF);
                        // Pixel 1
                        ptr.add(4).write_volatile(b);
                        ptr.add(5).write_volatile(g);
                        ptr.add(6).write_volatile(r);
                        ptr.add(7).write_volatile(0xFF);
                    }
                }
            } else {
                let fb_x = off_x + x;
                let fb_y = off_y + y;
                let offset = (fb_y * fb.stride + fb_x * fb.bpp) as usize;
                unsafe {
                    let ptr = fb.base.add(offset);
                    ptr.add(0).write_volatile(b);
                    ptr.add(1).write_volatile(g);
                    ptr.add(2).write_volatile(r);
                    ptr.add(3).write_volatile(0xFF);
                }
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn DG_SleepMs(ms: u32) {
    let start = arch::read_time();
    let target = start + (ms as usize) * 10_000; // 10MHz timer
    while arch::read_time() < target {
        // Poll keyboard during sleep so we don't miss events
        crate::virtio::input::poll();
        core::hint::spin_loop();
    }
}

#[no_mangle]
pub extern "C" fn DG_GetTicksMs() -> u32 {
    (arch::read_time() / 10_000) as u32
}

#[no_mangle]
pub extern "C" fn DG_GetKey(pressed: *mut i32, key: *mut u8) -> i32 {
    match pop_key_event() {
        Some((p, k)) => {
            unsafe {
                *pressed = p;
                *key = k;
            }
            1
        }
        None => 0,
    }
}

#[no_mangle]
pub extern "C" fn DG_SetWindowTitle(title: *const i8) {
    // Print title to UART for debugging
    if !title.is_null() {
        crate::print!("[doom] ");
        unsafe {
            let mut p = title;
            while *p != 0 {
                uart::putc(*p as u8);
                p = p.add(1);
            }
        }
        crate::println!();
    }
}

// ─── Entry point ─────────────────────────────────────────────────────────

extern "C" {
    fn doomgeneric_Create(argc: i32, argv: *const *const u8);
    fn doomgeneric_Tick();
}

pub fn start() {
    crate::println!("[doom] Starting DOOM...");
    crate::println!("[doom] WAD size: {} bytes", DOOM1_WAD.len());

    // Set doom mode flag
    unsafe { DOOM_MODE = true; }

    // Clear framebuffer to black
    if let Some(fb) = framebuffer::get() {
        fb.fill(framebuffer::Pixel::new(0, 0, 0));
    }

    // Set up argv for Doom
    // doomgeneric_Create expects (int argc, char **argv)
    let arg0 = b"doom\0".as_ptr();
    let arg1 = b"-iwad\0".as_ptr();
    let arg2 = b"doom1.wad\0".as_ptr();
    let argv: [*const u8; 3] = [arg0, arg1, arg2];

    unsafe {
        doomgeneric_Create(3, argv.as_ptr());
    }

    // Doom main loop
    loop {
        unsafe { doomgeneric_Tick(); }
    }
}
