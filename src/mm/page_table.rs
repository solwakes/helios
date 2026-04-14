/// Sv39 page table types and identity-map construction for Helios.

use core::alloc::Layout;
use alloc::alloc::alloc_zeroed;

// ---------------------------------------------------------------------------
// PTE flag bits
// ---------------------------------------------------------------------------
pub const PTE_V: u64 = 1 << 0; // Valid
pub const PTE_R: u64 = 1 << 1; // Read
pub const PTE_W: u64 = 1 << 2; // Write
pub const PTE_X: u64 = 1 << 3; // Execute
pub const PTE_U: u64 = 1 << 4; // User
pub const PTE_G: u64 = 1 << 5; // Global
pub const PTE_A: u64 = 1 << 6; // Accessed
pub const PTE_D: u64 = 1 << 7; // Dirty

/// Number of entries per page table (4096 / 8).
const PT_ENTRIES: usize = 512;

/// A single Sv39 page table entry.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct PageTableEntry(pub u64);

impl PageTableEntry {
    /// Create an invalid (zero) entry.
    pub const fn zero() -> Self {
        Self(0)
    }

    /// Create a leaf PTE that identity-maps a region.
    /// `ppn` is the physical page number (phys_addr >> 12).
    pub const fn leaf(ppn: u64, flags: u64) -> Self {
        Self((ppn << 10) | flags | PTE_V | PTE_A | PTE_D)
    }

    /// Create a non-leaf PTE pointing at the next-level page table.
    /// `ppn` is the physical page number of the child table.
    pub const fn branch(ppn: u64) -> Self {
        Self((ppn << 10) | PTE_V)
    }
}

/// A 4 KiB-aligned page table (512 × 8-byte entries).
#[repr(C, align(4096))]
pub struct PageTable {
    pub entries: [PageTableEntry; PT_ENTRIES],
}

// ---------------------------------------------------------------------------
// Allocation helper — grab a zeroed, page-aligned 4 KiB block from the heap.
// ---------------------------------------------------------------------------
fn alloc_page_table() -> &'static mut PageTable {
    let layout = Layout::from_size_align(4096, 4096).unwrap();
    let ptr = unsafe { alloc_zeroed(layout) };
    assert!(!ptr.is_null(), "alloc_page_table: out of memory");
    unsafe { &mut *(ptr as *mut PageTable) }
}

// ---------------------------------------------------------------------------
// Build identity-mapped root page table
// ---------------------------------------------------------------------------

/// Build the Sv39 root page table with identity mapping.
///
/// Layout:
///   - Level-2 entry 0 (VA 0x0000_0000..0x4000_0000): 1 GiB gigapage, RW
///     (covers MMIO: UART 0x1000_0000, fw_cfg 0x1010_0000, etc.)
///   - Level-2 entry 2 (VA 0x8000_0000..0xC000_0000): pointer → level-1 table
///     Level-1 entries 0..63: 2 MiB megapages, RWX (covers 128 MiB RAM)
///
/// Returns the physical address of the root page table.
pub fn build_identity_map() -> usize {
    // Allocate the root (level-2) page table
    let root = alloc_page_table();

    // --- MMIO gigapage: VA 0x0..0x4000_0000 ---------------------------------
    // Level-2 index 0, 1 GiB gigapage (leaf at level 2)
    // PPN for phys 0x0 is 0.
    let mmio_flags = PTE_R | PTE_W | PTE_G;
    root.entries[0] = PageTableEntry::leaf(0, mmio_flags);

    crate::println!(
        "[mm] Identity mapping MMIO: 0x0..0x40000000 (1 GiB gigapage, RW)"
    );

    // --- RAM megapages: VA 0x8000_0000..0x8800_0000 --------------------------
    // Level-2 index 2 (VPN[2] = 0x8000_0000 >> 30 = 2)
    let l1 = alloc_page_table();
    let l1_phys = l1 as *const PageTable as usize;
    root.entries[2] = PageTableEntry::branch((l1_phys >> 12) as u64);

    let ram_flags = PTE_R | PTE_W | PTE_X | PTE_G;
    // 64 × 2 MiB megapages starting at 0x8000_0000
    for i in 0u64..64 {
        // Physical address of this megapage: 0x8000_0000 + i * 2 MiB
        let phys = 0x8000_0000u64 + i * 0x20_0000;
        let ppn = phys >> 12;
        l1.entries[i as usize] = PageTableEntry::leaf(ppn, ram_flags);
    }

    crate::println!(
        "[mm] Identity mapping RAM: 0x80000000..0x88000000 (64 x 2 MiB megapages, RWX)"
    );

    let root_phys = root as *const PageTable as usize;
    root_phys
}
