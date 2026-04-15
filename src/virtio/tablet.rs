/// VirtIO tablet (absolute pointer) driver for Helios.
/// Receives absolute coordinate events from QEMU's virtio-tablet-device (device ID 18).
/// Provides cursor position and click events for the graph navigator.

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
const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_ABS: u16 = 3;

/// ABS axis codes
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;

/// Mouse button codes
const BTN_LEFT: u16 = 0x110;

/// Tablet coordinate range (QEMU virtio-tablet reports 0-32767)
const TABLET_MAX: u32 = 32767;

/// Framebuffer dimensions for scaling
const FB_WIDTH: u32 = 1024;
const FB_HEIGHT: u32 = 768;

/// Number of pre-allocated event buffers in the event queue.
const EVENT_BUF_COUNT: usize = 16;

/// Global cursor state
pub struct CursorState {
    pub x: u32,
    pub y: u32,
    pub left_pressed: bool,
    /// Set to true when a click event just happened (cleared after processing)
    pub left_clicked: bool,
    /// Set to true when the cursor position changed
    pub moved: bool,
    /// Pending raw coordinates from tablet (before sync)
    pending_x: Option<u32>,
    pending_y: Option<u32>,
}

impl CursorState {
    const fn new() -> Self {
        Self {
            x: FB_WIDTH / 2,
            y: FB_HEIGHT / 2,
            left_pressed: false,
            left_clicked: false,
            moved: false,
            pending_x: None,
            pending_y: None,
        }
    }
}

static mut CURSOR: CursorState = CursorState::new();

/// Get the current cursor state.
#[allow(static_mut_refs)]
pub fn cursor() -> &'static CursorState {
    unsafe { &CURSOR }
}

/// Get mutable cursor state.
#[allow(static_mut_refs)]
pub fn cursor_mut() -> &'static mut CursorState {
    unsafe { &mut CURSOR }
}

/// Clear the "just clicked" flag after it has been consumed.
pub fn clear_click() {
    unsafe { CURSOR.left_clicked = false; }
}

/// Clear the "moved" flag after it has been consumed.
pub fn clear_moved() {
    unsafe { CURSOR.moved = false; }
}

pub struct VirtioTablet {
    mmio: VirtioMmio,
    eventq: Virtqueue,
    event_bufs: *mut VirtioInputEvent,
}

/// Global tablet device instance.
static mut TABLET_DEV: Option<VirtioTablet> = None;


/// Get the MMIO base address of the tablet device (so the keyboard driver can skip it).
pub fn tablet_base() -> Option<usize> {
    unsafe { TABLET_DEV.as_ref().map(|d| d.mmio.base) }
}

/// Initialize the global tablet device. Returns true if found.
pub fn init() -> bool {
    match VirtioTablet::init() {
        Some(tab) => {
            crate::println!("[tablet] VirtIO tablet initialized");
            unsafe { TABLET_DEV = Some(tab); }
            true
        }
        None => {
            crate::println!("[tablet] No VirtIO tablet found");
            false
        }
    }
}

/// Poll the tablet device for events. Returns number of events processed.
#[allow(static_mut_refs)]
pub fn poll() -> usize {
    let dev = match unsafe { TABLET_DEV.as_mut() } {
        Some(d) => d,
        None => return 0,
    };
    dev.poll()
}

/// VirtIO input config space offsets (relative to 0x100 device config area)
const VIRTIO_INPUT_CFG_SELECT: usize = 0x00;
const VIRTIO_INPUT_CFG_SUBSEL: usize = 0x01;
const VIRTIO_INPUT_CFG_SIZE: usize = 0x02;
// const VIRTIO_INPUT_CFG_DATA: usize = 0x08;

/// VirtIO input config select values
const VIRTIO_INPUT_CFG_EV_BITS: u8 = 0x11;

/// Check if a VirtIO input device supports a given event type.
/// Queries config space: select=EV_BITS, subsel=event_type, then reads size.
fn device_supports_event(mmio: &VirtioMmio, event_type: u8) -> bool {
    mmio.write_config_u8(VIRTIO_INPUT_CFG_SELECT, VIRTIO_INPUT_CFG_EV_BITS);
    mmio.write_config_u8(VIRTIO_INPUT_CFG_SUBSEL, event_type);
    // Memory fence to ensure writes are visible
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    let size = mmio.read_config_u8(VIRTIO_INPUT_CFG_SIZE);
    size > 0
}

impl VirtioTablet {
    pub fn init() -> Option<Self> {
        // Find an input device (ID 18) that supports EV_ABS events (tablet/mouse)
        // Try both possible input devices (index 0 and 1)
        let mut mmio = None;
        for n in 0..4usize {
            if let Some(candidate) = VirtioMmio::probe_nth(18, n) {
                // Check if this device supports EV_ABS (absolute positioning)
                // We need to partially init the device first for config reads to work
                candidate.init_device();
                let has_abs = device_supports_event(&candidate, EV_ABS as u8);
                crate::println!(
                    "[tablet] Input device #{} @ {:#x}: EV_ABS={}",
                    n, candidate.base, has_abs
                );
                if has_abs {
                    mmio = Some(candidate);
                    break;
                }
                // Reset this device since we don't want it
                unsafe {
                    core::ptr::write_volatile((candidate.base + 0x070) as *mut u32, 0);
                }
            }
        }
        let mmio = mmio?;
        crate::println!(
            "[tablet] Found tablet device @ {:#x} (version {})",
            mmio.base,
            mmio.version
        );

        // Device already initialized by the probe loop above

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
            crate::println!("[tablet] Failed to allocate event buffers");
            return None;
        }

        let mut dev = VirtioTablet {
            mmio,
            eventq,
            event_bufs,
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
        crate::println!("[tablet] Device ready, queue size={}, buffers={}", qs, buf_count);

        Some(dev)
    }

    fn enqueue_event_buf(&mut self, i: usize) {
        let desc_idx = match self.eventq.alloc_desc() {
            Some(d) => d,
            None => return,
        };

        let buf_addr = unsafe { self.event_bufs.add(i) } as u64;
        let buf_len = core::mem::size_of::<VirtioInputEvent>() as u32;

        self.eventq.set_desc(desc_idx, buf_addr, buf_len, VRING_DESC_F_WRITE, 0);
        self.eventq.push_avail(desc_idx);
    }

    fn poll(&mut self) -> usize {
        let mut count = 0;

        while let Some(elem) = self.eventq.poll_used() {
            self.mmio.ack_interrupt();

            let desc_idx = elem.id as u16;

            let event = unsafe {
                let d = self.eventq.desc.add(desc_idx as usize);
                let addr = ptr::read_volatile(&(*d).addr);
                ptr::read_volatile(addr as *const VirtioInputEvent)
            };

            // Process the event
            unsafe {
                match event.type_ {
                    EV_ABS => {
                        match event.code {
                            ABS_X => {
                                CURSOR.pending_x = Some(event.value);
                            }
                            ABS_Y => {
                                CURSOR.pending_y = Some(event.value);
                            }
                            _ => {}
                        }
                        count += 1;
                    }
                    EV_KEY => {
                        if event.code == BTN_LEFT {
                            let was_pressed = CURSOR.left_pressed;
                            CURSOR.left_pressed = event.value != 0;
                            // Click = transition from not pressed to pressed
                            if !was_pressed && CURSOR.left_pressed {
                                CURSOR.left_clicked = true;
                            }
                            count += 1;
                        }
                    }
                    EV_SYN => {
                        // Apply pending coordinates on sync
                        if let Some(raw_x) = CURSOR.pending_x {
                            CURSOR.pending_x = None;
                            let new_x = (raw_x as u64 * FB_WIDTH as u64 / TABLET_MAX as u64) as u32;
                            let new_x = new_x.min(FB_WIDTH - 1);
                            if new_x != CURSOR.x {
                                CURSOR.x = new_x;
                                CURSOR.moved = true;
                            }
                        }
                        if let Some(raw_y) = CURSOR.pending_y {
                            CURSOR.pending_y = None;
                            let new_y = (raw_y as u64 * FB_HEIGHT as u64 / TABLET_MAX as u64) as u32;
                            let new_y = new_y.min(FB_HEIGHT - 1);
                            if new_y != CURSOR.y {
                                CURSOR.y = new_y;
                                CURSOR.moved = true;
                            }
                        }
                    }
                    _ => {}
                }
            }

            // Recycle the descriptor
            let buf_addr = unsafe {
                let d = self.eventq.desc.add(desc_idx as usize);
                ptr::read_volatile(&(*d).addr)
            };
            let base = self.event_bufs as u64;
            let event_size = core::mem::size_of::<VirtioInputEvent>() as u64;
            let buf_idx = ((buf_addr - base) / event_size) as usize;

            self.eventq.free_desc(desc_idx);
            self.enqueue_event_buf(buf_idx);
        }

        if count > 0 {
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            unsafe { core::arch::asm!("fence iorw, iorw"); }
            self.mmio.notify(0);
        }

        count
    }
}
