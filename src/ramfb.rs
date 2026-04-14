/// ramfb driver for Helios.
///
/// ramfb is a simple QEMU display device: allocate a framebuffer in RAM,
/// write a config struct via fw_cfg, and QEMU displays it.

use crate::fwcfg;
use core::alloc::Layout;

/// DRM_FORMAT_XRGB8888 — "XR24" in little-endian ASCII
const DRM_FORMAT_XRGB8888: u32 = 0x34325258;

const DEFAULT_WIDTH: u32 = 1024;
const DEFAULT_HEIGHT: u32 = 768;
const BPP: u32 = 4;

pub struct RamfbInfo {
    pub fb_ptr: *mut u8,
    pub width: u32,
    pub height: u32,
}

pub fn init() -> Option<RamfbInfo> {
    if !fwcfg::verify_signature() {
        crate::println!("[ramfb] fw_cfg device not found");
        return None;
    }
    crate::println!("[ramfb] fw_cfg device verified (QEMU signature OK)");

    let (selector, file_size) = match fwcfg::find_file("etc/ramfb") {
        Some(f) => f,
        None => {
            crate::println!("[ramfb] etc/ramfb not found — is -device ramfb enabled?");
            return None;
        }
    };
    crate::println!("[ramfb] Found etc/ramfb (selector={:#x}, size={})", selector, file_size);

    // Allocate framebuffer
    let fb_size = (DEFAULT_WIDTH * DEFAULT_HEIGHT * BPP) as usize;
    let layout = Layout::from_size_align(fb_size, 4096).ok()?;
    let fb_ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
    if fb_ptr.is_null() {
        crate::println!("[ramfb] Failed to allocate framebuffer");
        return None;
    }
    let fb_phys = fb_ptr as u64;
    crate::println!("[ramfb] Framebuffer at {:#x} ({} KiB)", fb_phys, fb_size / 1024);

    // Build RAMFBCfg as big-endian bytes on the HEAP (for DMA)
    let cfg_layout = Layout::from_size_align(28, 16).ok()?;
    let cfg_ptr = unsafe { alloc::alloc::alloc_zeroed(cfg_layout) };
    if cfg_ptr.is_null() {
        crate::println!("[ramfb] Failed to allocate config");
        return None;
    }

    let stride = DEFAULT_WIDTH * BPP;
    unsafe {
        core::ptr::copy_nonoverlapping(fb_phys.to_be_bytes().as_ptr(), cfg_ptr, 8);          // addr
        core::ptr::copy_nonoverlapping(DRM_FORMAT_XRGB8888.to_be_bytes().as_ptr(), cfg_ptr.add(8), 4);  // fourcc
        core::ptr::copy_nonoverlapping(0u32.to_be_bytes().as_ptr(), cfg_ptr.add(12), 4);     // flags
        core::ptr::copy_nonoverlapping(DEFAULT_WIDTH.to_be_bytes().as_ptr(), cfg_ptr.add(16), 4);  // width
        core::ptr::copy_nonoverlapping(DEFAULT_HEIGHT.to_be_bytes().as_ptr(), cfg_ptr.add(20), 4); // height
        core::ptr::copy_nonoverlapping(stride.to_be_bytes().as_ptr(), cfg_ptr.add(24), 4);   // stride
    }

    // Write via DMA
    if !fwcfg::dma_write(selector, cfg_ptr, 28) {
        crate::println!("[ramfb] DMA write failed");
        return None;
    }

    crate::println!("[ramfb] Configuration written ({}x{} XRGB8888)", DEFAULT_WIDTH, DEFAULT_HEIGHT);

    Some(RamfbInfo {
        fb_ptr,
        width: DEFAULT_WIDTH,
        height: DEFAULT_HEIGHT,
    })
}
