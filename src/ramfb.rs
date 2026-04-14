/// ramfb driver for Helios.
///
/// ramfb is a simple QEMU display device: allocate a framebuffer in RAM,
/// write a config struct via fw_cfg, and QEMU displays it. No virtqueues needed.

use crate::fwcfg;
use core::alloc::Layout;

/// DRM_FORMAT_XRGB8888 — "XR24" in little-endian ASCII
const DRM_FORMAT_XRGB8888: u32 = 0x34325258;

const DEFAULT_WIDTH: u32 = 1024;
const DEFAULT_HEIGHT: u32 = 768;
const BPP: u32 = 4;

/// RAMFBCfg — all fields big-endian.
#[repr(C, align(4))]
struct RamfbCfg {
    addr: u64,
    fourcc: u32,
    flags: u32,
    width: u32,
    height: u32,
    stride: u32,
}

/// Result of ramfb initialization.
pub struct RamfbInfo {
    pub fb_ptr: *mut u8,
    pub width: u32,
    pub height: u32,
}

/// Initialize the ramfb device. Returns framebuffer info on success.
pub fn init() -> Option<RamfbInfo> {
    // Verify fw_cfg device
    if !fwcfg::verify_signature() {
        crate::println!("[ramfb] fw_cfg device not found");
        return None;
    }
    crate::println!("[ramfb] fw_cfg device verified (QEMU signature OK)");

    // Find etc/ramfb file
    let (selector, _size) = match fwcfg::find_file("etc/ramfb") {
        Some(f) => f,
        None => {
            crate::println!("[ramfb] etc/ramfb not found in fw_cfg — is -device ramfb enabled?");
            return None;
        }
    };
    crate::println!("[ramfb] Found etc/ramfb (selector={:#x})", selector);

    // Allocate framebuffer memory
    let fb_size = (DEFAULT_WIDTH * DEFAULT_HEIGHT * BPP) as usize;
    let layout = Layout::from_size_align(fb_size, 4096).ok()?;
    let fb_ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
    if fb_ptr.is_null() {
        crate::println!("[ramfb] Failed to allocate framebuffer ({} bytes)", fb_size);
        return None;
    }
    let fb_phys = fb_ptr as u64;
    crate::println!(
        "[ramfb] Framebuffer allocated at {:#x} ({} KiB)",
        fb_phys,
        fb_size / 1024
    );

    // Build RAMFBCfg — all fields in big-endian
    let cfg = RamfbCfg {
        addr: fb_phys.to_be(),
        fourcc: DRM_FORMAT_XRGB8888.to_be(),
        flags: 0u32.to_be(),
        width: DEFAULT_WIDTH.to_be(),
        height: DEFAULT_HEIGHT.to_be(),
        stride: (DEFAULT_WIDTH * BPP).to_be(),
    };

    // Write config via fw_cfg DMA
    let cfg_ptr = &cfg as *const RamfbCfg as *const u8;
    let cfg_len = core::mem::size_of::<RamfbCfg>() as u32;

    if !fwcfg::dma_write(selector, cfg_ptr, cfg_len) {
        crate::println!("[ramfb] fw_cfg DMA write failed");
        return None;
    }
    crate::println!("[ramfb] Configuration written successfully ({}x{} XRGB8888)", DEFAULT_WIDTH, DEFAULT_HEIGHT);

    Some(RamfbInfo {
        fb_ptr,
        width: DEFAULT_WIDTH,
        height: DEFAULT_HEIGHT,
    })
}
