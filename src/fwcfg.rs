/// fw_cfg MMIO driver for QEMU virt (RISC-V).
///
/// The fw_cfg device provides a simple interface for the guest to read/write
/// configuration data. On RISC-V virt, it's at MMIO address 0x10100000.

use core::ptr;

const FWCFG_BASE: usize = 0x1010_0000;
const FWCFG_DATA: usize = FWCFG_BASE + 0x00;
const FWCFG_SEL: usize = FWCFG_BASE + 0x08;
const FWCFG_DMA: usize = FWCFG_BASE + 0x10;

const FWCFG_SIG_SELECT: u16 = 0x0000;
const FWCFG_DIR_SELECT: u16 = 0x0019;

/// A fw_cfg file directory entry.
#[derive(Clone, Copy)]
pub struct FwCfgFile {
    pub size: u32,
    pub select: u16,
    pub name: [u8; 56],
}

/// DMA access descriptor — must be 16-byte aligned.
#[repr(C, align(16))]
struct FwCfgDmaAccess {
    control: u32,
    length: u32,
    address: u64,
}

/// Select a fw_cfg entry by writing to the selector register.
fn select(entry: u16) {
    unsafe {
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

    // Read 4-byte big-endian count
    let mut count_buf = [0u8; 4];
    read_bytes(&mut count_buf);
    let count = u32::from_be_bytes(count_buf);

    for _ in 0..count {
        // Each entry: 4-byte size (BE), 2-byte select (BE), 2-byte reserved, 56-byte name
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

        // Compare name (null-terminated)
        let fname_len = fname.iter().position(|&b| b == 0).unwrap_or(56);
        let fname_str = &fname[..fname_len];
        if fname_str == name.as_bytes() {
            return Some((sel, size));
        }
    }
    None
}

/// Write data to a fw_cfg file entry via DMA.
///
/// `file_select` is the selector for the file (from find_file).
/// `data` is a pointer to the data buffer in guest physical memory.
/// `len` is the number of bytes to write.
///
/// Safety: `data` must point to valid guest-physical memory of at least `len` bytes.
/// The DMA descriptor is allocated on the stack with proper alignment.
pub fn dma_write(file_select: u16, data: *const u8, len: u32) -> bool {
    // Build the DMA access descriptor
    // control: bits[31:16] = selector, bit 4 = select, bit 1 = write
    let control: u32 = ((file_select as u32) << 16) | (1 << 4) | (1 << 1);

    let mut dma = FwCfgDmaAccess {
        control: control.to_be(),
        length: len.to_be(),
        address: (data as u64).to_be(),
    };

    // Write the physical address of the DMA descriptor to the DMA register
    let dma_addr = &mut dma as *mut FwCfgDmaAccess as u64;

    // Ensure all memory writes are visible before triggering DMA
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

    unsafe {
        ptr::write_volatile(FWCFG_DMA as *mut u64, dma_addr.to_be());
    }

    // Poll until control field clears (DMA complete)
    // The device zeroes control on success, sets bit 0 on error
    for _ in 0..1_000_000 {
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        let ctrl = u32::from_be(unsafe {
            ptr::read_volatile(&dma.control as *const u32)
        });
        if ctrl == 0 {
            return true;
        }
        if ctrl & 1 != 0 {
            return false; // error
        }
        core::hint::spin_loop();
    }
    false // timeout
}
