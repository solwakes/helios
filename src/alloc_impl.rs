/// Linked-list free-list heap allocator for Helios.
/// Replaces the previous bump allocator with proper deallocation and coalescing.
/// Uses a statically reserved heap region defined in the linker script.
/// Single-hart only — no synchronization needed.

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;

extern "C" {
    static _heap_start: u8;
    static _heap_end: u8;
}

/// A free block in the free list, stored in-place in free memory.
/// Minimum block size = size_of::<FreeBlock>() = 16 bytes on 64-bit.
#[repr(C)]
struct FreeBlock {
    size: usize,
    next: *mut FreeBlock,
}

const MIN_BLOCK_SIZE: usize = core::mem::size_of::<FreeBlock>(); // 16
const ALIGN: usize = MIN_BLOCK_SIZE; // 16-byte minimum alignment

struct LinkedListAllocator {
    head: UnsafeCell<*mut FreeBlock>,
    initialized: UnsafeCell<bool>,
}

// Safety: single-hart kernel, no concurrent access.
unsafe impl Sync for LinkedListAllocator {}

#[global_allocator]
static ALLOCATOR: LinkedListAllocator = LinkedListAllocator {
    head: UnsafeCell::new(core::ptr::null_mut()),
    initialized: UnsafeCell::new(false),
};

fn heap_start() -> usize {
    unsafe { &_heap_start as *const u8 as usize }
}

fn heap_end() -> usize {
    unsafe { &_heap_end as *const u8 as usize }
}

/// Align `addr` up to `align` (must be a power of two).
#[inline]
fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}

/// Initialize the free list. MUST be called before any allocations.
pub fn alloc_init() {
    unsafe {
        let start = align_up(heap_start(), ALIGN);
        let end = heap_end();
        let size = end - start;

        let block = start as *mut FreeBlock;
        (*block).size = size;
        (*block).next = core::ptr::null_mut();

        *ALLOCATOR.head.get() = block;
        *ALLOCATOR.initialized.get() = true;
    }
}

/// Walk the free list and sum free block sizes.
pub fn heap_free() -> usize {
    unsafe {
        let mut total = 0usize;
        let mut current = *ALLOCATOR.head.get();
        while !current.is_null() {
            total += (*current).size;
            current = (*current).next;
        }
        total
    }
}

/// Return the number of heap bytes currently in use (total - free).
pub fn heap_used() -> usize {
    heap_total() - heap_free()
}

/// Return the total heap size in bytes.
pub fn heap_total() -> usize {
    heap_end() - heap_start()
}

/// Return the heap start address.
pub fn heap_start_addr() -> usize {
    heap_start()
}

/// Return the heap end address.
pub fn heap_end_addr() -> usize {
    heap_end()
}

unsafe impl GlobalAlloc for LinkedListAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if !*self.initialized.get() {
            // Fallback: auto-init if not yet initialized (shouldn't happen)
            alloc_init();
        }

        let align = if layout.align() > ALIGN { layout.align() } else { ALIGN };
        let size = align_up(layout.size(), align);
        let needed = if size < MIN_BLOCK_SIZE { MIN_BLOCK_SIZE } else { size };

        // Walk free list (first-fit), keeping track of previous block for removal.
        let mut prev: *mut FreeBlock = core::ptr::null_mut();
        let mut current = *self.head.get();

        while !current.is_null() {
            let block_addr = current as usize;
            let block_size = (*current).size;

            // Calculate the aligned start address within this block
            let aligned_addr = align_up(block_addr, align);
            let padding = aligned_addr - block_addr;
            let total_needed = needed + padding;

            if block_size >= total_needed {
                // This block is large enough.

                if padding >= MIN_BLOCK_SIZE {
                    // There's enough room before the aligned address to keep a free block.
                    // Split the front portion off as a separate free block.
                    let front_block = current;
                    (*front_block).size = padding;
                    // front_block stays in the list in place of current

                    // The allocated portion starts at aligned_addr
                    let remaining_after = block_size - padding - needed;

                    if remaining_after >= MIN_BLOCK_SIZE {
                        // Split: create a new free block after the allocation
                        let new_block = (aligned_addr + needed) as *mut FreeBlock;
                        (*new_block).size = remaining_after;
                        (*new_block).next = (*current).next;
                        (*front_block).next = new_block;
                    } else {
                        // Use the rest (no split after)
                        // front_block.next already points to current.next
                        // but we need to keep it pointing to current's next
                        (*front_block).next = (*current).next;
                    }

                    return aligned_addr as *mut u8;
                } else if padding == 0 {
                    // Block is already aligned. Simple case.
                    let remaining = block_size - needed;

                    if remaining >= MIN_BLOCK_SIZE {
                        // Split: create a new free block after the allocation
                        let new_block = (block_addr + needed) as *mut FreeBlock;
                        (*new_block).size = remaining;
                        (*new_block).next = (*current).next;

                        // Replace current in the list with new_block
                        if prev.is_null() {
                            *self.head.get() = new_block;
                        } else {
                            (*prev).next = new_block;
                        }
                    } else {
                        // Use the entire block (no split)
                        if prev.is_null() {
                            *self.head.get() = (*current).next;
                        } else {
                            (*prev).next = (*current).next;
                        }
                    }

                    return block_addr as *mut u8;
                } else {
                    // padding > 0 but < MIN_BLOCK_SIZE: we can't create a free block
                    // from the padding. We need to consume the padding too.
                    // Check if the block can satisfy needed starting from block_addr
                    // (i.e., waste the padding by allocating from block start).
                    // Actually, we need alignment, so we can't return block_addr.
                    // Skip this block and try the next one.
                    // (This wastes some space but keeps the algorithm simple.)
                    prev = current;
                    current = (*current).next;
                    continue;
                }
            }

            prev = current;
            current = (*current).next;
        }

        // No suitable block found
        core::ptr::null_mut()
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = self.alloc(layout);
        if !ptr.is_null() {
            // Zero with volatile writes — prevents LLVM from optimising
            // this into a memset call (which would recurse infinitely
            // due to the compiler-builtins memset bug on RISC-V).
            let size = layout.size();
            let mut i = 0usize;
            let aligned = size & !7;
            while i < aligned {
                (ptr.add(i) as *mut u64).write_volatile(0);
                i += 8;
            }
            while i < size {
                ptr.add(i).write_volatile(0);
                i += 1;
            }
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let align = if layout.align() > ALIGN { layout.align() } else { ALIGN };
        let size = align_up(layout.size(), align);
        let freed_size = if size < MIN_BLOCK_SIZE { MIN_BLOCK_SIZE } else { size };
        let freed_addr = ptr as usize;

        // Create a new free block at the freed address
        let freed_block = freed_addr as *mut FreeBlock;
        (*freed_block).size = freed_size;
        (*freed_block).next = core::ptr::null_mut();

        // Insert into free list sorted by address, then coalesce
        let head = self.head.get();

        if (*head).is_null() || freed_addr < *head as usize {
            // Insert at the beginning
            (*freed_block).next = *head;
            *head = freed_block;

            // Try to coalesce with next
            coalesce_with_next(freed_block);
            return;
        }

        // Find the right position (sorted by address)
        let mut current = *head;
        while !(*current).next.is_null() && ((*current).next as usize) < freed_addr {
            current = (*current).next;
        }

        // Insert after current
        (*freed_block).next = (*current).next;
        (*current).next = freed_block;

        // Coalesce freed_block with its next neighbor first
        coalesce_with_next(freed_block);
        // Then coalesce current with freed_block (which may have grown)
        coalesce_with_next(current);
    }
}

/// If `block` and the block after it are adjacent in memory, merge them.
unsafe fn coalesce_with_next(block: *mut FreeBlock) {
    if block.is_null() {
        return;
    }
    let next = (*block).next;
    if next.is_null() {
        return;
    }
    let block_end = (block as usize) + (*block).size;
    if block_end == next as usize {
        // Adjacent — merge
        (*block).size += (*next).size;
        (*block).next = (*next).next;
    }
}
