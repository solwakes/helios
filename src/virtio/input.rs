/// VirtIO input device driver for Helios.
/// Receives keyboard events from QEMU's virtio-keyboard-device (device ID 18).
/// Translates Linux keycodes to ASCII/escape sequences and feeds them to the shell.

use super::mmio::VirtioMmio;
use super::{Virtqueue, VirtqAvail, VirtqDesc, VirtqUsed, VRING_DESC_F_WRITE};
use alloc::alloc::alloc_zeroed;
use core::alloc::Layout;
use core::ptr;

/// VirtIO input event (8 bytes), matches Linux evdev `input_event` layout.
#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioInputEvent {
    type_: u16,
    code: u16,
    value: u32,
}

/// Linux evdev event types
const EV_KEY: u16 = 1;

/// Number of pre-allocated event buffers in the event queue.
const EVENT_BUF_COUNT: usize = 16;

pub struct VirtioInput {
    mmio: VirtioMmio,
    eventq: Virtqueue,
    /// Pre-allocated event buffers (heap, for DMA).
    event_bufs: *mut VirtioInputEvent,
    /// Track shift key state.
    shift_held: bool,
}

/// Global input device instance.
static mut INPUT_DEV: Option<VirtioInput> = None;

/// Initialize the global input device. Returns true if found.
pub fn init() -> bool {
    match VirtioInput::init() {
        Some(inp) => {
            crate::println!("[input] VirtIO keyboard initialized");
            unsafe { INPUT_DEV = Some(inp); }
            true
        }
        None => {
            crate::println!("[input] No VirtIO keyboard found");
            false
        }
    }
}

/// Get the MMIO base address of the keyboard device (so the tablet driver can skip it).
pub fn keyboard_base() -> Option<usize> {
    unsafe { INPUT_DEV.as_ref().map(|d| d.mmio.base) }
}

/// Poll the input device and feed bytes to the shell.
/// Returns the number of key events processed.
#[allow(static_mut_refs)]
pub fn poll() -> usize {
    let dev = match unsafe { INPUT_DEV.as_mut() } {
        Some(d) => d,
        None => return 0,
    };
    dev.poll()
}

impl VirtioInput {
    /// Probe, initialize, and set up the input device.
    /// Finds the first input device (ID 18) that is NOT the already-claimed tablet device.
    pub fn init() -> Option<Self> {
        // Skip the tablet device if already initialized
        let tablet_base = super::tablet::tablet_base().unwrap_or(0);
        let mmio = if tablet_base != 0 {
            VirtioMmio::probe_skip(18, tablet_base)?
        } else {
            VirtioMmio::probe(18)?
        };
        crate::println!(
            "[input] Found keyboard device @ {:#x} (version {})",
            mmio.base, mmio.version
        );

        mmio.init_device();

        // Device already initialized above

        // Set up event queue (virtq 0)
        let (dp, ap, up, qs) = mmio.setup_queue(0)?;
        let eventq = Virtqueue::new(
            dp as *mut VirtqDesc,
            ap as *mut VirtqAvail,
            up as *mut VirtqUsed,
            qs,
        );

        // Allocate event buffers for DMA
        let buf_count = (qs as usize).min(EVENT_BUF_COUNT);
        let layout = Layout::from_size_align(
            buf_count * core::mem::size_of::<VirtioInputEvent>(),
            16,
        )
        .unwrap();
        let event_bufs = unsafe { alloc_zeroed(layout) } as *mut VirtioInputEvent;
        if event_bufs.is_null() {
            crate::println!("[input] Failed to allocate event buffers");
            return None;
        }

        let mut dev = VirtioInput {
            mmio,
            eventq,
            event_bufs,
            shift_held: false,
        };

        // Pre-fill the event queue with device-writable buffers
        for i in 0..buf_count {
            dev.enqueue_event_buf(i);
        }

        // Notify device that buffers are available
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        unsafe { core::arch::asm!("fence iorw, iorw"); }
        dev.mmio.notify(0);

        dev.mmio.driver_ok();
        crate::println!("[input] Device ready, queue size={}, buffers={}", qs, buf_count);

        Some(dev)
    }

    /// Enqueue a single event buffer at index `i` into the available ring.
    fn enqueue_event_buf(&mut self, i: usize) {
        let desc_idx = match self.eventq.alloc_desc() {
            Some(d) => d,
            None => return,
        };

        let buf_addr = unsafe { self.event_bufs.add(i) } as u64;
        let buf_len = core::mem::size_of::<VirtioInputEvent>() as u32;

        // Device-writable buffer (device writes events into it)
        self.eventq.set_desc(desc_idx, buf_addr, buf_len, VRING_DESC_F_WRITE, 0);
        self.eventq.push_avail(desc_idx);
    }

    /// Poll the used ring for completed events, translate keycodes, and feed to shell.
    fn poll(&mut self) -> usize {
        let mut count = 0;

        while let Some(elem) = self.eventq.poll_used() {
            self.mmio.ack_interrupt();

            let desc_idx = elem.id as u16;

            // Read the event from the descriptor's buffer address
            let event = unsafe {
                let d = self.eventq.desc.add(desc_idx as usize);
                let addr = ptr::read_volatile(&(*d).addr);
                ptr::read_volatile(addr as *const VirtioInputEvent)
            };

            // Process key events
            if event.type_ == EV_KEY {
                self.handle_key(event.code, event.value);
                count += 1;
            }

            // Recycle: free the descriptor and re-enqueue a buffer
            // Find which buffer index this descriptor was pointing to
            let buf_addr = unsafe {
                let d = self.eventq.desc.add(desc_idx as usize);
                ptr::read_volatile(&(*d).addr)
            };
            let base = self.event_bufs as u64;
            let event_size = core::mem::size_of::<VirtioInputEvent>() as u64;
            let buf_idx = ((buf_addr - base) / event_size) as usize;

            // Free descriptor back to free list
            self.eventq.free_desc(desc_idx);

            // Re-enqueue this buffer
            self.enqueue_event_buf(buf_idx);
        }

        // If we processed any events, notify device of newly available buffers
        if count > 0 {
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            unsafe { core::arch::asm!("fence iorw, iorw"); }
            self.mmio.notify(0);
        }

        count
    }

    /// Handle a single key event.
    fn handle_key(&mut self, code: u16, value: u32) {
        // In DOOM mode, forward raw keycodes (both press and release) to doom
        if crate::doom::is_doom_mode() {
            // value: 1=press, 0=release, 2=repeat (treat repeat as press)
            if value == 2 {
                return; // ignore repeat in doom mode
            }
            let doom_key = crate::doom::evdev_to_doom(code);
            if doom_key != 0 {
                crate::doom::push_key_event(value == 1, doom_key);
            }
            return;
        }

        // Track shift state on both press and release
        if code == KEY_LEFTSHIFT || code == KEY_RIGHTSHIFT {
            self.shift_held = value != 0; // 1=press, 0=release
            return;
        }

        // Only process key press events (value=1), ignore release (0) and repeat (2)
        if value != 1 {
            return;
        }

        // Arrow keys → escape sequences
        match code {
            KEY_UP => {
                feed_bytes(&[0x1b, b'[', b'A']);
                return;
            }
            KEY_DOWN => {
                feed_bytes(&[0x1b, b'[', b'B']);
                return;
            }
            KEY_RIGHT => {
                feed_bytes(&[0x1b, b'[', b'C']);
                return;
            }
            KEY_LEFT => {
                feed_bytes(&[0x1b, b'[', b'D']);
                return;
            }
            _ => {}
        }

        // Translate keycode to ASCII
        if let Some(byte) = self.keycode_to_ascii(code) {
            crate::shell::process_byte(byte);
        }
    }

    /// Translate a Linux keycode to an ASCII byte, applying shift state.
    fn keycode_to_ascii(&self, code: u16) -> Option<u8> {
        // Special keys first
        match code {
            KEY_ESC => return Some(0x1b),
            KEY_BACKSPACE => return Some(0x7f),
            KEY_TAB => return Some(b'\t'),
            KEY_ENTER => return Some(b'\r'),
            KEY_SPACE => return Some(b' '),
            _ => {}
        }

        // Letter keys: A-Z
        if code >= KEY_A_START && code <= KEY_A_START + 25 {
            // The keycode mapping isn't contiguous A-Z, use lookup table
        }

        // Use lookup tables
        if self.shift_held {
            keycode_to_shifted(code)
        } else {
            keycode_to_normal(code)
        }
    }
}

/// Feed multiple bytes to the shell (for escape sequences).
fn feed_bytes(bytes: &[u8]) {
    for &b in bytes {
        crate::shell::process_byte(b);
    }
}

// ── Linux keycodes ──────────────────────────────────────────────────────────

const KEY_ESC: u16 = 1;
const KEY_BACKSPACE: u16 = 14;
const KEY_TAB: u16 = 15;
const KEY_ENTER: u16 = 28;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_RIGHTSHIFT: u16 = 54;
const KEY_SPACE: u16 = 57;
const KEY_UP: u16 = 103;
const KEY_LEFT: u16 = 105;
const KEY_RIGHT: u16 = 106;
const KEY_DOWN: u16 = 108;
const KEY_A_START: u16 = 30; // not used directly, see tables

// ── Keycode translation tables ──────────────────────────────────────────────

/// Normal (unshifted) keycode → ASCII. Returns None for unmapped keys.
fn keycode_to_normal(code: u16) -> Option<u8> {
    match code {
        2 => Some(b'1'),
        3 => Some(b'2'),
        4 => Some(b'3'),
        5 => Some(b'4'),
        6 => Some(b'5'),
        7 => Some(b'6'),
        8 => Some(b'7'),
        9 => Some(b'8'),
        10 => Some(b'9'),
        11 => Some(b'0'),
        12 => Some(b'-'),
        13 => Some(b'='),
        16 => Some(b'q'),
        17 => Some(b'w'),
        18 => Some(b'e'),
        19 => Some(b'r'),
        20 => Some(b't'),
        21 => Some(b'y'),
        22 => Some(b'u'),
        23 => Some(b'i'),
        24 => Some(b'o'),
        25 => Some(b'p'),
        26 => Some(b'['),
        27 => Some(b']'),
        30 => Some(b'a'),
        31 => Some(b's'),
        32 => Some(b'd'),
        33 => Some(b'f'),
        34 => Some(b'g'),
        35 => Some(b'h'),
        36 => Some(b'j'),
        37 => Some(b'k'),
        38 => Some(b'l'),
        39 => Some(b';'),
        40 => Some(b'\''),
        41 => Some(b'`'),
        43 => Some(b'\\'),
        44 => Some(b'z'),
        45 => Some(b'x'),
        46 => Some(b'c'),
        47 => Some(b'v'),
        48 => Some(b'b'),
        49 => Some(b'n'),
        50 => Some(b'm'),
        51 => Some(b','),
        52 => Some(b'.'),
        53 => Some(b'/'),
        _ => None,
    }
}

/// Shifted keycode → ASCII.
fn keycode_to_shifted(code: u16) -> Option<u8> {
    match code {
        2 => Some(b'!'),
        3 => Some(b'@'),
        4 => Some(b'#'),
        5 => Some(b'$'),
        6 => Some(b'%'),
        7 => Some(b'^'),
        8 => Some(b'&'),
        9 => Some(b'*'),
        10 => Some(b'('),
        11 => Some(b')'),
        12 => Some(b'_'),
        13 => Some(b'+'),
        16 => Some(b'Q'),
        17 => Some(b'W'),
        18 => Some(b'E'),
        19 => Some(b'R'),
        20 => Some(b'T'),
        21 => Some(b'Y'),
        22 => Some(b'U'),
        23 => Some(b'I'),
        24 => Some(b'O'),
        25 => Some(b'P'),
        26 => Some(b'{'),
        27 => Some(b'}'),
        30 => Some(b'A'),
        31 => Some(b'S'),
        32 => Some(b'D'),
        33 => Some(b'F'),
        34 => Some(b'G'),
        35 => Some(b'H'),
        36 => Some(b'J'),
        37 => Some(b'K'),
        38 => Some(b'L'),
        39 => Some(b':'),
        40 => Some(b'"'),
        41 => Some(b'~'),
        43 => Some(b'|'),
        44 => Some(b'Z'),
        45 => Some(b'X'),
        46 => Some(b'C'),
        47 => Some(b'V'),
        48 => Some(b'B'),
        49 => Some(b'N'),
        50 => Some(b'M'),
        51 => Some(b'<'),
        52 => Some(b'>'),
        53 => Some(b'?'),
        _ => None,
    }
}
