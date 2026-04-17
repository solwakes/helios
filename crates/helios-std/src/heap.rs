//! Fixed-size bump allocator that lives inside the task's binary image.
//!
//! # Why a bump allocator
//!
//! M31 is the first Rust-in-U-mode milestone. We want `alloc::String`,
//! `alloc::format!`, and `alloc::Vec` to Just Work so user code reads
//! like idiomatic Rust. There is as yet no `SYS_MAP_NODE` syscall for
//! asking the kernel for anonymous heap pages, so the heap has to live
//! inside memory the task already owns.
//!
//! The current kernel (M31) maps the whole binary image as R+W+X+U —
//! see `build_user_address_space` in `src/user.rs`. This waives W^X
//! *within* a single task in exchange for being able to put a
//! mutable `static` heap buffer directly in the binary's `.data`
//! section. Cross-task caps are still strictly MMU-enforced (no edge →
//! no mapping → no access); the waived bit is task-internal.
//!
//! # Why `[0xAA; N]` rather than `[0; N]`
//!
//! A `static mut X: [u8; N] = [0; N]` with a zero initializer lands in
//! `.bss` (which `-O binary` objcopy *drops* from the raw binary). The
//! kernel then wouldn't see those bytes in the blob and the first
//! write would go to whatever happened to be at that VA (usually
//! freshly-zeroed mapped memory, but fragile).
//!
//! Explicitly pre-filling with a non-zero byte forces the array into
//! `.data`, which IS part of the image the kernel copies into user
//! memory. The bump allocator treats these bytes as uninitialized;
//! users that read before writing get `0xAA`, which is both
//! distinctive in a debugger and a cheap tripwire for buggy
//! read-before-write code.
//!
//! # Capacity
//!
//! 64 KiB. Plenty for demo programs that build a few `String`s and a
//! `Vec` or two. A proper slab / free-list allocator (or, better, a
//! kernel-backed anonymous-page allocator) lands in a future
//! milestone once `SYS_MAP_NODE` exists.

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicUsize, Ordering};

/// Size of the bump region, in bytes. 64 KiB is enough for demo
/// programs; expand if needed in a future milestone.
pub const HEAP_SIZE: usize = 64 * 1024;

/// 16-byte-aligned heap wrapper so any reasonable Rust alignment fits
/// with at most +15 bytes of internal padding.
#[repr(C, align(16))]
struct Heap {
    bytes: UnsafeCell<[u8; HEAP_SIZE]>,
}

// SAFETY: M31 runs one HART in U-mode with interrupts off in user
// code; there's no preemption, no other task can see this `static`.
// The atomic cursor serialises concurrent hypothetical callers anyway.
unsafe impl Sync for Heap {}

/// Heap buffer. Forced non-zero-init so it lands in `.data` (not
/// `.bss`) and ends up in the raw binary image.
#[used]
#[link_section = ".data.helios_heap"]
static HEAP: Heap = Heap {
    bytes: UnsafeCell::new([0xAA; HEAP_SIZE]),
};

/// Next free offset within `HEAP`, monotonically increasing.
static NEXT: AtomicUsize = AtomicUsize::new(0);

struct BumpAllocator;

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let base = HEAP.bytes.get() as usize;
        let size = layout.size();
        let align = layout.align().max(1);

        // Single-HART U-mode: `Relaxed` is sound. Use CAS so a
        // hypothetical future reentrant caller (e.g. signal) doesn't
        // double-allocate the same bytes.
        let mut cur = NEXT.load(Ordering::Relaxed);
        loop {
            let start = base.wrapping_add(cur);
            let aligned = (start + align - 1) & !(align - 1);
            let end = match aligned.checked_add(size) {
                Some(e) => e,
                None => return null_mut(),
            };
            let new_next = end - base;
            if new_next > HEAP_SIZE {
                return null_mut();
            }
            match NEXT.compare_exchange(
                cur,
                new_next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return aligned as *mut u8,
                Err(observed) => cur = observed,
            }
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump: free is a no-op. Deferred until a real allocator lands.
    }
}

#[global_allocator]
static GLOBAL: BumpAllocator = BumpAllocator;

/// Current heap high-water mark, in bytes. Handy for diagnostics —
/// the hello demo prints this.
pub fn used() -> usize {
    NEXT.load(Ordering::Relaxed)
}

/// Total heap capacity (always [`HEAP_SIZE`]).
pub const fn capacity() -> usize {
    HEAP_SIZE
}
