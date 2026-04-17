/// Memory management for Helios — Sv39 page tables + virtual memory.

pub mod page_table;

use crate::arch::riscv64 as arch;

/// Sv39 mode value for the `satp` CSR (bits 63:60 = 8).
pub const SATP_MODE_SV39: usize = 8 << 60;

/// Physical address of the kernel root page table, captured at init so
/// later code (user-space setup) can clone kernel mappings into per-task
/// address spaces.
static mut KERNEL_ROOT_PA: usize = 0;

/// Return the physical address of the kernel root page table.
#[allow(static_mut_refs)]
pub fn kernel_root_pa() -> usize {
    unsafe { KERNEL_ROOT_PA }
}

/// Initialise virtual memory: build identity-mapped Sv39 page tables and
/// enable paging by writing to `satp`.
pub fn init() {
    crate::println!("[mm] Setting up Sv39 page tables...");

    let root_phys = page_table::build_identity_map();
    unsafe { KERNEL_ROOT_PA = root_phys; }
    let root_ppn = root_phys >> 12;
    let satp_val = SATP_MODE_SV39 | root_ppn;

    crate::println!("[mm] Enabling Sv39 paging (satp = {:#018x})", satp_val);

    // Flush TLB before switching, write satp, flush again.
    arch::sfence_vma();
    arch::write_satp(satp_val);
    arch::sfence_vma();

    crate::println!("[mm] Paging enabled successfully.");
}
