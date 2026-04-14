/// VirtIO MMIO transport driver.
/// Supports both legacy (v1) and modern (v2) transports.

use core::ptr;

// ── Register offsets (common) ────────────────────────────────────────────────
const MAGIC_VALUE: usize = 0x000;
const VERSION: usize = 0x004;
const DEVICE_ID: usize = 0x008;
#[allow(dead_code)]
const VENDOR_ID: usize = 0x00c;
const DEVICE_FEATURES: usize = 0x010;
const DEVICE_FEATURES_SEL: usize = 0x014;
const DRIVER_FEATURES: usize = 0x020;
const DRIVER_FEATURES_SEL: usize = 0x024;
const QUEUE_SEL: usize = 0x030;
const QUEUE_NUM_MAX: usize = 0x034;
const QUEUE_NUM: usize = 0x038;
const QUEUE_NOTIFY: usize = 0x050;
const INTERRUPT_STATUS: usize = 0x060;
const INTERRUPT_ACK: usize = 0x064;
const STATUS: usize = 0x070;

// ── v2 (modern) only ─────────────────────────────────────────────────────────
const QUEUE_READY: usize = 0x044;
const QUEUE_DESC_LOW: usize = 0x080;
const QUEUE_DESC_HIGH: usize = 0x084;
const QUEUE_DRIVER_LOW: usize = 0x090;
const QUEUE_DRIVER_HIGH: usize = 0x094;
const QUEUE_DEVICE_LOW: usize = 0x0a0;
const QUEUE_DEVICE_HIGH: usize = 0x0a4;

// ── v1 (legacy) only ─────────────────────────────────────────────────────────
const GUEST_PAGE_SIZE: usize = 0x028;
const QUEUE_ALIGN: usize = 0x03c;
const QUEUE_PFN: usize = 0x040;

// ── Status bits ──────────────────────────────────────────────────────────────
const STATUS_ACK: u32 = 1;
const STATUS_DRIVER: u32 = 2;
const STATUS_DRIVER_OK: u32 = 4;
const STATUS_FEATURES_OK: u32 = 8;
const STATUS_FAILED: u32 = 128;

const VIRTIO_MAGIC: u32 = 0x7472_6976; // "virt"

// ── QEMU virt machine MMIO layout ───────────────────────────────────────────
const VIRTIO_MMIO_BASE: usize = 0x1000_1000;
const VIRTIO_MMIO_COUNT: usize = 8;
const VIRTIO_MMIO_STRIDE: usize = 0x1000;

pub struct VirtioMmio {
    pub base: usize,
    pub version: u32,
}

impl VirtioMmio {
    #[inline]
    fn read32(&self, offset: usize) -> u32 {
        unsafe { ptr::read_volatile((self.base + offset) as *const u32) }
    }

    #[inline]
    fn write32(&self, offset: usize, val: u32) {
        unsafe { ptr::write_volatile((self.base + offset) as *mut u32, val) }
    }

    /// Probe all MMIO slots for a device with the given ID.
    pub fn probe(device_id: u32) -> Option<Self> {
        for i in 0..VIRTIO_MMIO_COUNT {
            let base = VIRTIO_MMIO_BASE + i * VIRTIO_MMIO_STRIDE;
            let magic = unsafe { ptr::read_volatile(base as *const u32) };
            if magic != VIRTIO_MAGIC {
                continue;
            }
            let version = unsafe { ptr::read_volatile((base + VERSION) as *const u32) };
            let dev_id = unsafe { ptr::read_volatile((base + DEVICE_ID) as *const u32) };

            crate::println!(
                "[virtio] MMIO slot {} @ {:#x}: version={}, device_id={}",
                i,
                base,
                version,
                dev_id
            );

            if dev_id == device_id {
                return Some(VirtioMmio { base, version });
            }
        }
        None
    }

    /// Run the standard device init sequence (reset → ack → driver → features).
    pub fn init_device(&self) {
        // Reset
        self.write32(STATUS, 0);

        // Acknowledge
        self.write32(STATUS, STATUS_ACK);

        // Driver
        self.write32(STATUS, STATUS_ACK | STATUS_DRIVER);

        if self.version >= 2 {
            // Modern feature negotiation
            self.write32(DEVICE_FEATURES_SEL, 0);
            let _features = self.read32(DEVICE_FEATURES);
            // We don't need any features for basic GPU operation
            self.write32(DRIVER_FEATURES_SEL, 0);
            self.write32(DRIVER_FEATURES, 0);

            self.write32(
                STATUS,
                STATUS_ACK | STATUS_DRIVER | STATUS_FEATURES_OK,
            );
            let s = self.read32(STATUS);
            if s & STATUS_FEATURES_OK == 0 {
                crate::println!("[virtio] Device rejected features!");
                self.write32(STATUS, STATUS_FAILED);
            }
        } else {
            // Legacy: set guest page size, negotiate features
            self.write32(GUEST_PAGE_SIZE, 4096);
            let features = self.read32(DEVICE_FEATURES);
            crate::println!("[virtio] Device features[0]: {:#010x}", features);
            // Accept all device features for now
            self.write32(DRIVER_FEATURES, features);

            // Check features word 1 (bits 32+)
            self.write32(DEVICE_FEATURES_SEL, 1);
            let features1 = self.read32(DEVICE_FEATURES);
            crate::println!("[virtio] Device features[1]: {:#010x}", features1);
            // Accept VIRTIO_F_VERSION_1 (bit 32 = bit 0 of word 1)
            self.write32(DRIVER_FEATURES_SEL, 1);
            self.write32(DRIVER_FEATURES, features1);
            // Reset sel back to 0
            self.write32(DEVICE_FEATURES_SEL, 0);
            self.write32(DRIVER_FEATURES_SEL, 0);
        }
    }

    /// Mark the device DRIVER_OK so it starts processing queues.
    pub fn driver_ok(&self) {
        if self.version >= 2 {
            self.write32(
                STATUS,
                STATUS_ACK | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK,
            );
        } else {
            self.write32(STATUS, STATUS_ACK | STATUS_DRIVER | STATUS_DRIVER_OK);
        }
    }

    /// Allocate and configure a virtqueue.
    /// Returns (desc_ptr, avail_ptr, used_ptr, negotiated_size).
    pub fn setup_queue(&self, queue_idx: u32) -> Option<(*mut u8, *mut u8, *mut u8, u16)> {
        self.write32(QUEUE_SEL, queue_idx);

        let max_size = self.read32(QUEUE_NUM_MAX);
        if max_size == 0 {
            crate::println!("[virtio] Queue {} not available", queue_idx);
            return None;
        }

        let queue_size = max_size.min(16) as u16;
        crate::println!(
            "[virtio] Queue {}: max={}, using={}",
            queue_idx,
            max_size,
            queue_size
        );

        self.write32(QUEUE_NUM, queue_size as u32);

        let n = queue_size as usize;
        let desc_bytes = n * 16;
        let avail_bytes = 6 + 2 * n;
        let used_bytes = 6 + 8 * n;

        if self.version >= 2 {
            // Modern: three separate address registers
            let total = align_up(desc_bytes, 16)
                + align_up(avail_bytes, 4)
                + align_up(used_bytes, 4);
            let layout =
                core::alloc::Layout::from_size_align(align_up(total, 4096), 4096).unwrap();
            let buf = unsafe { alloc::alloc::alloc_zeroed(layout) };
            if buf.is_null() {
                crate::println!("[virtio] Failed to allocate queue memory");
                return None;
            }

            let desc_ptr = buf;
            let avail_ptr = unsafe { buf.add(align_up(desc_bytes, 16)) };
            let used_off = align_up(desc_bytes, 16) + align_up(avail_bytes, 4);
            let used_ptr = unsafe { buf.add(used_off) };

            let da = desc_ptr as u64;
            let aa = avail_ptr as u64;
            let ua = used_ptr as u64;

            self.write32(QUEUE_DESC_LOW, da as u32);
            self.write32(QUEUE_DESC_HIGH, (da >> 32) as u32);
            self.write32(QUEUE_DRIVER_LOW, aa as u32);
            self.write32(QUEUE_DRIVER_HIGH, (aa >> 32) as u32);
            self.write32(QUEUE_DEVICE_LOW, ua as u32);
            self.write32(QUEUE_DEVICE_HIGH, (ua >> 32) as u32);
            self.write32(QUEUE_READY, 1);

            Some((desc_ptr, avail_ptr, used_ptr, queue_size))
        } else {
            // Legacy: contiguous layout. Used ring starts at next align boundary after desc+avail.
            let align = 4096usize;
            let page1 = align_up(desc_bytes + avail_bytes, align);
            let total = page1 + align_up(used_bytes, align);
            crate::println!("[virtio]   alloc {} bytes (page1={}, total={})", total, page1, total);
            let layout =
                core::alloc::Layout::from_size_align(total, 4096).unwrap();
            let buf = unsafe { alloc::alloc::alloc(layout) };
            crate::println!("[virtio]   alloc returned {:#x}", buf as usize);
            if !buf.is_null() {
                // Zero manually using volatile writes to avoid memset issues
                for i in 0..total {
                    unsafe { buf.add(i).write_volatile(0); }
                }
                crate::println!("[virtio]   zeroed OK");
            }
            if buf.is_null() {
                crate::println!("[virtio] Failed to allocate queue memory");
                return None;
            }

            self.write32(QUEUE_ALIGN, align as u32);
            let pfn = (buf as usize) / 4096;
            self.write32(QUEUE_PFN, pfn as u32);

            let desc_ptr = buf;
            let avail_ptr = unsafe { buf.add(desc_bytes) };
            let used_ptr = unsafe { buf.add(page1) };

            crate::println!(
                "[virtio]   desc={:#x} avail={:#x} used={:#x} pfn={:#x}",
                desc_ptr as usize, avail_ptr as usize, used_ptr as usize, pfn
            );

            Some((desc_ptr, avail_ptr, used_ptr, queue_size))
        }
    }

    /// Notify the device that queue `queue_idx` has new buffers.
    pub fn notify(&self, queue_idx: u32) {
        self.write32(QUEUE_NOTIFY, queue_idx);
    }

    /// Acknowledge pending interrupts.
    pub fn ack_interrupt(&self) {
        let s = self.read32(INTERRUPT_STATUS);
        if s != 0 {
            self.write32(INTERRUPT_ACK, s);
        }
    }
}

fn align_up(val: usize, align: usize) -> usize {
    (val + align - 1) & !(align - 1)
}
