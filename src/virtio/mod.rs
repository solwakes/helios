/// VirtIO common types and virtqueue implementation.

pub mod blk;
pub mod gpu;
pub mod mmio;

use core::ptr;

/// VirtIO descriptor flags
pub const VRING_DESC_F_NEXT: u16 = 1;
pub const VRING_DESC_F_WRITE: u16 = 2;

/// VirtIO descriptor (16 bytes)
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtqDesc {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

/// Available ring header
#[repr(C)]
pub struct VirtqAvail {
    pub flags: u16,
    pub idx: u16,
    // ring: [u16; N] follows
}

/// Used ring element
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtqUsedElem {
    pub id: u32,
    pub len: u32,
}

/// Used ring header
#[repr(C)]
pub struct VirtqUsed {
    pub flags: u16,
    pub idx: u16,
    // ring: [VirtqUsedElem; N] follows
}

/// A virtqueue with a small fixed size
pub struct Virtqueue {
    pub desc: *mut VirtqDesc,
    pub avail: *mut VirtqAvail,
    pub used: *mut VirtqUsed,
    pub queue_size: u16,
    pub free_head: u16,
    pub num_free: u16,
    pub last_used_idx: u16,
}

impl Virtqueue {
    /// Create a new virtqueue from pre-allocated, zeroed memory regions.
    pub fn new(
        desc: *mut VirtqDesc,
        avail: *mut VirtqAvail,
        used: *mut VirtqUsed,
        queue_size: u16,
    ) -> Self {
        // Build free descriptor chain
        for i in 0..queue_size {
            unsafe {
                let d = desc.add(i as usize);
                ptr::write_volatile(&mut (*d).flags, 0);
                ptr::write_volatile(
                    &mut (*d).next,
                    if i + 1 < queue_size { i + 1 } else { 0 },
                );
            }
        }
        unsafe {
            ptr::write_volatile(&mut (*avail).flags, 0);
            ptr::write_volatile(&mut (*avail).idx, 0);
            ptr::write_volatile(&mut (*used).flags, 0);
            ptr::write_volatile(&mut (*used).idx, 0);
        }

        Self {
            desc,
            avail,
            used,
            queue_size,
            free_head: 0,
            num_free: queue_size,
            last_used_idx: 0,
        }
    }

    /// Allocate a descriptor from the free list.
    pub fn alloc_desc(&mut self) -> Option<u16> {
        if self.num_free == 0 {
            return None;
        }
        let idx = self.free_head;
        unsafe {
            self.free_head = ptr::read_volatile(&(*self.desc.add(idx as usize)).next);
        }
        self.num_free -= 1;
        Some(idx)
    }

    /// Return a descriptor to the free list.
    pub fn free_desc(&mut self, idx: u16) {
        unsafe {
            let d = self.desc.add(idx as usize);
            ptr::write_volatile(&mut (*d).flags, 0);
            ptr::write_volatile(&mut (*d).next, self.free_head);
        }
        self.free_head = idx;
        self.num_free += 1;
    }

    /// Write a descriptor entry.
    pub fn set_desc(&mut self, idx: u16, addr: u64, len: u32, flags: u16, next: u16) {
        unsafe {
            let d = self.desc.add(idx as usize);
            ptr::write_volatile(&mut (*d).addr, addr);
            ptr::write_volatile(&mut (*d).len, len);
            ptr::write_volatile(&mut (*d).flags, flags);
            ptr::write_volatile(&mut (*d).next, next);
        }
    }

    /// Push a descriptor head into the available ring.
    pub fn push_avail(&mut self, head: u16) {
        unsafe {
            let idx = ptr::read_volatile(&(*self.avail).idx);
            // ring[] starts right after the flags + idx fields (4 bytes)
            let ring_base = (self.avail as *mut u8).add(4) as *mut u16;
            let slot = (idx % self.queue_size) as usize;
            ptr::write_volatile(ring_base.add(slot), head);
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            ptr::write_volatile(&mut (*self.avail).idx, idx.wrapping_add(1));
        }
    }

    /// Poll the used ring for a completed entry.
    pub fn poll_used(&mut self) -> Option<VirtqUsedElem> {
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        unsafe {
            let used_idx = ptr::read_volatile(&(*self.used).idx);
            if self.last_used_idx == used_idx {
                return None;
            }
            let ring_base = (self.used as *mut u8).add(4) as *mut VirtqUsedElem;
            let slot = (self.last_used_idx % self.queue_size) as usize;
            let elem = ptr::read_volatile(ring_base.add(slot));
            self.last_used_idx = self.last_used_idx.wrapping_add(1);
            Some(elem)
        }
    }
}
