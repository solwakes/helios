/// VirtIO block device driver for Helios.
/// Uses a single requestq (queue 0) with 3-descriptor chains:
///   header (read-only) → data (read/write) → status (write-only)

use super::mmio::VirtioMmio;
use super::{Virtqueue, VirtqAvail, VirtqDesc, VirtqUsed, VRING_DESC_F_NEXT, VRING_DESC_F_WRITE};
use alloc::alloc::{alloc, alloc_zeroed, dealloc};
use core::alloc::Layout;

const VIRTIO_BLK_T_IN: u32 = 0;  // read
const VIRTIO_BLK_T_OUT: u32 = 1; // write

const VIRTIO_BLK_S_OK: u8 = 0;

/// VirtIO block request header (16 bytes).
#[repr(C)]
struct VirtioBlkReqHeader {
    type_: u32,
    reserved: u32,
    sector: u64,
}

pub struct VirtioBlk {
    mmio: VirtioMmio,
    requestq: Virtqueue,
}

/// Global block device instance.
static mut BLOCK_DEV: Option<VirtioBlk> = None;

/// Initialize the global block device. Returns true if found.
pub fn init() -> bool {
    match VirtioBlk::init() {
        Some(blk) => {
            crate::println!("[blk] VirtIO block device initialized");
            unsafe { BLOCK_DEV = Some(blk); }
            true
        }
        None => {
            crate::println!("[blk] No VirtIO block device found");
            false
        }
    }
}

/// Get a mutable reference to the global block device.
#[allow(static_mut_refs)]
pub fn get_mut() -> Option<&'static mut VirtioBlk> {
    unsafe { BLOCK_DEV.as_mut() }
}

/// Check if block device is present.
#[allow(static_mut_refs)]
pub fn is_present() -> bool {
    unsafe { BLOCK_DEV.is_some() }
}

impl VirtioBlk {
    /// Probe, initialize, and set up the block device. Returns None if not found.
    pub fn init() -> Option<Self> {
        let mmio = VirtioMmio::probe(2)?; // device ID 2 = block
        crate::println!(
            "[blk] Found block device @ {:#x} (version {})",
            mmio.base,
            mmio.version
        );

        mmio.init_device();

        let (dp, ap, up, qs) = mmio.setup_queue(0)?;
        let requestq = Virtqueue::new(
            dp as *mut VirtqDesc,
            ap as *mut VirtqAvail,
            up as *mut VirtqUsed,
            qs,
        );

        mmio.driver_ok();
        crate::println!("[blk] Device ready, queue size={}", qs);

        Some(VirtioBlk { mmio, requestq })
    }

    /// Perform a single-sector block I/O operation.
    /// `is_write`: true for write, false for read.
    /// `sector`: sector number (512 bytes each).
    /// `data`: pointer to a heap-allocated 512-byte buffer.
    fn do_request(&mut self, is_write: bool, sector: u64, data: *mut u8) -> bool {
        // Heap-allocate the header
        let header_layout = Layout::from_size_align(
            core::mem::size_of::<VirtioBlkReqHeader>(), 16
        ).unwrap();
        let header_ptr = unsafe { alloc(header_layout) } as *mut VirtioBlkReqHeader;
        if header_ptr.is_null() {
            return false;
        }
        unsafe {
            core::ptr::write_volatile(&mut (*header_ptr).type_, if is_write { VIRTIO_BLK_T_OUT } else { VIRTIO_BLK_T_IN });
            core::ptr::write_volatile(&mut (*header_ptr).reserved, 0);
            core::ptr::write_volatile(&mut (*header_ptr).sector, sector);
        }

        // Heap-allocate the status byte
        let status_layout = Layout::from_size_align(1, 16).unwrap();
        let status_ptr = unsafe { alloc(status_layout) };
        if status_ptr.is_null() {
            unsafe { dealloc(header_ptr as *mut u8, header_layout); }
            return false;
        }
        unsafe { *status_ptr = 0xFF; } // sentinel

        // Allocate 3 descriptors
        let d0 = match self.requestq.alloc_desc() { Some(d) => d, None => return false };
        let d1 = match self.requestq.alloc_desc() { Some(d) => d, None => return false };
        let d2 = match self.requestq.alloc_desc() { Some(d) => d, None => return false };

        // Descriptor 0: header (device-readable)
        self.requestq.set_desc(
            d0,
            header_ptr as u64,
            core::mem::size_of::<VirtioBlkReqHeader>() as u32,
            VRING_DESC_F_NEXT,
            d1,
        );

        // Descriptor 1: data buffer
        let data_flags = if is_write {
            VRING_DESC_F_NEXT // device-readable for writes
        } else {
            VRING_DESC_F_NEXT | VRING_DESC_F_WRITE // device-writable for reads
        };
        self.requestq.set_desc(d1, data as u64, 512, data_flags, d2);

        // Descriptor 2: status (device-writable)
        self.requestq.set_desc(d2, status_ptr as u64, 1, VRING_DESC_F_WRITE, 0);

        // Memory fence before making available
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        self.requestq.push_avail(d0);

        // Fence before notify
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        // RISC-V I/O fence
        unsafe { core::arch::asm!("fence iorw, iorw"); }

        self.mmio.notify(0);

        // Poll for completion
        let mut completed = false;
        for _ in 0..50_000_000u32 {
            if let Some(_elem) = self.requestq.poll_used() {
                self.mmio.ack_interrupt();
                completed = true;
                break;
            }
            core::hint::spin_loop();
        }

        // Read status
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        let status = unsafe { core::ptr::read_volatile(status_ptr) };

        // Free descriptors
        self.requestq.free_desc(d2);
        self.requestq.free_desc(d1);
        self.requestq.free_desc(d0);

        // Free heap allocations
        unsafe {
            dealloc(header_ptr as *mut u8, header_layout);
            dealloc(status_ptr, status_layout);
        }

        if !completed {
            crate::println!("[blk] Request timed out");
            return false;
        }

        status == VIRTIO_BLK_S_OK
    }

    /// Read a single 512-byte sector into buf.
    pub fn read_sector(&mut self, sector: u64, buf: &mut [u8; 512]) -> bool {
        // Heap-allocate data buffer for DMA
        let layout = Layout::from_size_align(512, 512).unwrap();
        let dma_buf = unsafe { alloc_zeroed(layout) };
        if dma_buf.is_null() {
            return false;
        }

        let ok = self.do_request(false, sector, dma_buf);

        if ok {
            // Copy from DMA buffer to caller's buffer
            unsafe {
                core::ptr::copy_nonoverlapping(dma_buf, buf.as_mut_ptr(), 512);
            }
        }

        unsafe { dealloc(dma_buf, layout); }
        ok
    }

    /// Write a single 512-byte sector from buf.
    pub fn write_sector(&mut self, sector: u64, buf: &[u8; 512]) -> bool {
        // Heap-allocate data buffer for DMA
        let layout = Layout::from_size_align(512, 512).unwrap();
        let dma_buf = unsafe { alloc(layout) };
        if dma_buf.is_null() {
            return false;
        }

        // Copy data into DMA buffer
        unsafe {
            core::ptr::copy_nonoverlapping(buf.as_ptr(), dma_buf, 512);
        }

        let ok = self.do_request(true, sector, dma_buf);

        unsafe { dealloc(dma_buf, layout); }
        ok
    }

    /// Read multiple sectors into a byte slice (must be sector-aligned in length).
    pub fn read(&mut self, sector: u64, buf: &mut [u8]) -> bool {
        let num_sectors = (buf.len() + 511) / 512;
        let mut sector_buf = [0u8; 512];
        for i in 0..num_sectors {
            if !self.read_sector(sector + i as u64, &mut sector_buf) {
                return false;
            }
            let offset = i * 512;
            let remaining = buf.len() - offset;
            let copy_len = if remaining < 512 { remaining } else { 512 };
            buf[offset..offset + copy_len].copy_from_slice(&sector_buf[..copy_len]);
        }
        true
    }

    /// Write multiple sectors from a byte slice.
    pub fn write(&mut self, sector: u64, buf: &[u8]) -> bool {
        let num_sectors = (buf.len() + 511) / 512;
        for i in 0..num_sectors {
            let mut sector_buf = [0u8; 512];
            let offset = i * 512;
            let remaining = buf.len() - offset;
            let copy_len = if remaining < 512 { remaining } else { 512 };
            sector_buf[..copy_len].copy_from_slice(&buf[offset..offset + copy_len]);
            if !self.write_sector(sector + i as u64, &sector_buf) {
                return false;
            }
        }
        true
    }
}
