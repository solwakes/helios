/// fw_cfg MMIO driver for QEMU virt (RISC-V).
///
/// MMIO layout (from QEMU hw/riscv/virt.c, DTB fw-cfg@10100000):
///   Data register:     0x10100000 (width 8, DEVICE_BIG_ENDIAN)
///   Selector register: 0x10100008 (width 2, DEVICE_BIG_ENDIAN)
///   DMA register:      0x10100010 (width 8, DEVICE_BIG_ENDIAN)

use core::ptr;

const FWCFG_BASE: usize = 0x1010_0000;
const FWCFG_DATA: usize = FWCFG_BASE + 0x00;
const FWCFG_SEL: usize = FWCFG_BASE + 0x08;
const FWCFG_DMA: usize = FWCFG_BASE + 0x10;

const FWCFG_SIG_SELECT: u16 = 0x0000;
const FWCFG_DIR_SELECT: u16 = 0x0019;

/// Select a fw_cfg entry by writing to the selector register.
fn select(entry: u16) {
    unsafe {
        // DEVICE_BIG_ENDIAN: QEMU byte-swaps 16-bit writes on LE targets.
        // We apply .to_be() so the double-swap gives the device the raw value.
        ptr::write_volatile(FWCFG_SEL as *mut u16, entry.to_be());
    }
}

/// Read a single byte from the data register.
fn read_byte() -> u8 {
    unsafe { ptr::read_volatile(FWCFG_DATA as *const u8) }
}

/// Read `n` bytes from the currently selected entry.
fn read_bytes(buf: &mut [u8]) {
    for b in buf.iter_mut() {
        *b = read_byte();
    }
}

/// Verify the fw_cfg device is present by checking its signature.
pub fn verify_signature() -> bool {
    select(FWCFG_SIG_SELECT);
    let mut sig = [0u8; 4];
    read_bytes(&mut sig);
    &sig == b"QEMU"
}

/// Find a fw_cfg file by name. Returns (select, size) if found.
pub fn find_file(name: &str) -> Option<(u16, u32)> {
    select(FWCFG_DIR_SELECT);

    let mut count_buf = [0u8; 4];
    read_bytes(&mut count_buf);
    let count = u32::from_be_bytes(count_buf);

    for _ in 0..count {
        let mut size_buf = [0u8; 4];
        read_bytes(&mut size_buf);
        let size = u32::from_be_bytes(size_buf);

        let mut sel_buf = [0u8; 2];
        read_bytes(&mut sel_buf);
        let sel = u16::from_be_bytes(sel_buf);

        let mut _reserved = [0u8; 2];
        read_bytes(&mut _reserved);

        let mut fname = [0u8; 56];
        read_bytes(&mut fname);

        let fname_len = fname.iter().position(|&b| b == 0).unwrap_or(56);
        let fname_str = &fname[..fname_len];
        if fname_str == name.as_bytes() {
            return Some((sel, size));
        }
    }
    None
}

/// Read data from a fw_cfg file entry via the data register.
pub fn read_file(file_select: u16, buf: &mut [u8]) {
    select(file_select);
    read_bytes(buf);
}

/// Write data to a fw_cfg file via DMA.
///
/// The DMA descriptor and data must be in guest-physical memory.
/// All DMA descriptor fields use big-endian encoding.
pub fn dma_write(file_select: u16, data: *const u8, len: u32) -> bool {
    // DMA control: selector in top 16 bits, SELECT=0x08, WRITE=0x10
    let control: u32 = ((file_select as u32) << 16) | 0x18;

    // Allocate the DMA descriptor on the heap for DMA safety (16-byte aligned)
    let layout = core::alloc::Layout::from_size_align(16, 16).unwrap();
    let dma_ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
    if dma_ptr.is_null() {
        return false;
    }

    // Write DMA descriptor fields in big-endian (as raw bytes)
    unsafe {
        // control: u32 BE at offset 0
        ptr::copy_nonoverlapping(control.to_be_bytes().as_ptr(), dma_ptr, 4);
        // length: u32 BE at offset 4
        ptr::copy_nonoverlapping(len.to_be_bytes().as_ptr(), dma_ptr.add(4), 4);
        // address: u64 BE at offset 8
        ptr::copy_nonoverlapping((data as u64).to_be_bytes().as_ptr(), dma_ptr.add(8), 8);
    }

    let dma_phys = dma_ptr as u64;

    // Ensure descriptor and data are visible before triggering DMA
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    // RISC-V fence for I/O ordering
    unsafe { core::arch::asm!("fence iorw, iorw") };

    // Write DMA descriptor address to the DMA register.
    // DEVICE_BIG_ENDIAN + our .to_be() = device sees raw address.
    unsafe {
        ptr::write_volatile(FWCFG_DMA as *mut u64, dma_phys.to_be());
    }

    // Poll until control field clears (DMA complete)
    for i in 0..1_000_000u32 {
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        // Read control field (first 4 bytes of DMA descriptor, big-endian)
        let raw = unsafe { ptr::read_volatile(dma_ptr as *const u32) };
        let ctrl = u32::from_be_bytes(raw.to_ne_bytes());
        if ctrl == 0 {
            return true;
        }
        if ctrl & 1 != 0 {
            crate::println!("[fwcfg] DMA error (ctrl={:#x})", ctrl);
            return false;
        }
        core::hint::spin_loop();
    }
    crate::println!("[fwcfg] DMA timeout");
    false
}
