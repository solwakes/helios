/// Simple bump allocator for Helios.
/// Uses a statically reserved heap region defined in the linker script.
/// Single-hart only — no synchronization needed.

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;

extern "C" {
    static _heap_start: u8;
    static _heap_end: u8;
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator {
    next: UnsafeCell::new(0),
};

struct BumpAllocator {
    next: UnsafeCell<usize>,
}

// Safety: single-hart kernel, no concurrent access.
unsafe impl Sync for BumpAllocator {}

fn heap_start() -> usize {
    unsafe { &_heap_start as *const u8 as usize }
}

fn heap_end() -> usize {
    unsafe { &_heap_end as *const u8 as usize }
}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let next = self.next.get();
        let mut current = *next;
        if current == 0 {
            current = heap_start();
        }
        let aligned = (current + layout.align() - 1) & !(layout.align() - 1);
        let new_next = aligned + layout.size();
        if new_next > heap_end() {
            return core::ptr::null_mut();
        }
        *next = new_next;
        aligned as *mut u8
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocator never frees
    }
}
