/// VirtIO GPU device driver — minimal implementation for framebuffer output.

use super::mmio::VirtioMmio;
use super::{Virtqueue, VRING_DESC_F_NEXT, VRING_DESC_F_WRITE};
use core::alloc::Layout;
use core::mem::size_of;

// ── GPU command types ────────────────────────────────────────────────────────
const VIRTIO_GPU_CMD_GET_DISPLAY_INFO: u32 = 0x0100;
const VIRTIO_GPU_CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
const VIRTIO_GPU_CMD_SET_SCANOUT: u32 = 0x0103;
const VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0104;
const VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
const VIRTIO_GPU_CMD_RESOURCE_FLUSH: u32 = 0x0106;

const VIRTIO_GPU_RESP_OK_NODATA: u32 = 0x1100;
const VIRTIO_GPU_RESP_OK_DISPLAY_INFO: u32 = 0x1101;

const VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM: u32 = 1;

// ── Wire structs (all #[repr(C)]) ────────────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy)]
struct CtrlHdr {
    type_: u32,
    flags: u32,
    fence_id: u64,
    ctx_id: u32,
    padding: u32,
}

impl CtrlHdr {
    const fn cmd(type_: u32) -> Self {
        Self { type_, flags: 0, fence_id: 0, ctx_id: 0, padding: 0 }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Rect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DisplayOne {
    r: Rect,
    enabled: u32,
    flags: u32,
}

#[repr(C)]
struct RespDisplayInfo {
    hdr: CtrlHdr,
    pmodes: [DisplayOne; 16],
}

#[repr(C)]
struct CmdResourceCreate2d {
    hdr: CtrlHdr,
    resource_id: u32,
    format: u32,
    width: u32,
    height: u32,
}

#[repr(C)]
struct CmdSetScanout {
    hdr: CtrlHdr,
    r: Rect,
    scanout_id: u32,
    resource_id: u32,
}

#[repr(C)]
struct MemEntry {
    addr: u64,
    length: u32,
    padding: u32,
}

#[repr(C)]
struct CmdAttachBacking {
    hdr: CtrlHdr,
    resource_id: u32,
    nr_entries: u32,
    entry: MemEntry, // inline single entry
}

#[repr(C)]
struct CmdTransferToHost2d {
    hdr: CtrlHdr,
    r: Rect,
    offset: u64,
    resource_id: u32,
    padding: u32,
}

#[repr(C)]
struct CmdResourceFlush {
    hdr: CtrlHdr,
    r: Rect,
    resource_id: u32,
    padding: u32,
}

// ── GPU driver ───────────────────────────────────────────────────────────────

pub struct VirtioGpu {
    mmio: VirtioMmio,
    controlq: Virtqueue,
    pub width: u32,
    pub height: u32,
    pub fb_ptr: *mut u8,
}

impl VirtioGpu {
    /// Probe, initialise, and set up the framebuffer.  Returns `None` if no GPU found.
    pub fn init() -> Option<Self> {
        crate::println!("[gpu] Probing for VirtIO GPU...");

        let mmio = VirtioMmio::probe(16)?; // device ID 16 = GPU
        crate::println!(
            "[gpu] Found GPU @ {:#x} (version {})",
            mmio.base,
            mmio.version
        );

        mmio.init_device();
        crate::println!("[gpu] Device init done, setting up queue...");

        let (dp, ap, up, qs) = mmio.setup_queue(0)?;
        crate::println!("[gpu] Queue memory allocated, initializing virtqueue...");

        let controlq = Virtqueue::new(
            dp as *mut super::VirtqDesc,
            ap as *mut super::VirtqAvail,
            up as *mut super::VirtqUsed,
            qs,
        );
        crate::println!("[gpu] Virtqueue initialized, setting DRIVER_OK...");

        mmio.driver_ok();
        crate::println!("[gpu] Device ready, controlq size={}", qs);

        let mut gpu = VirtioGpu {
            mmio,
            controlq,
            width: 0,
            height: 0,
            fb_ptr: core::ptr::null_mut(),
        };

        // 1. Display info — skip for now, hardcode to debug ATTACH_BACKING issue
        gpu.width = 1024;
        gpu.height = 768;
        crate::println!("[gpu] Using hardcoded {}x{}", gpu.width, gpu.height);

        // 2. Create resource 1
        gpu.cmd_resource_create_2d(1, gpu.width, gpu.height);

        // DEBUG: Try creating same resource again — should fail if first really succeeded
        crate::println!("[gpu] DEBUG: Sending duplicate CREATE_2D...");
        gpu.cmd_resource_create_2d(1, gpu.width, gpu.height);

        // 3. Allocate framebuffer
        let fb_size = (gpu.width as usize) * (gpu.height as usize) * 4;
        let layout = Layout::from_size_align(fb_size, 4096).unwrap();
        let fb = unsafe { alloc::alloc::alloc_zeroed(layout) };
        if fb.is_null() {
            crate::println!("[gpu] Framebuffer alloc failed ({} bytes)", fb_size);
            return None;
        }
        gpu.fb_ptr = fb;
        crate::println!(
            "[gpu] Framebuffer @ {:#x} ({} KiB)",
            fb as usize,
            fb_size / 1024
        );

        // 4. Attach backing
        gpu.cmd_attach_backing(1, fb as u64, fb_size as u32);

        // 5. Set scanout
        gpu.cmd_set_scanout(0, 1, gpu.width, gpu.height);

        crate::println!("[gpu] Setup complete: {}x{}", gpu.width, gpu.height);
        Some(gpu)
    }

    // ── low-level command submission ─────────────────────────────────────

    /// Send a command (read-only) + response (write-only) descriptor pair,
    /// poll until the device signals completion.
    fn send_cmd(&mut self, cmd: *const u8, cmd_len: u32, resp: *mut u8, resp_len: u32) {
        let d0 = self.controlq.alloc_desc().expect("virtq full");
        let d1 = self.controlq.alloc_desc().expect("virtq full");

        crate::println!(
            "[gpu]   send_cmd: d0={}, d1={}, cmd={:#x}, resp={:#x}",
            d0, d1, cmd as usize, resp as usize
        );

        self.controlq
            .set_desc(d0, cmd as u64, cmd_len, VRING_DESC_F_NEXT, d1);
        self.controlq
            .set_desc(d1, resp as u64, resp_len, VRING_DESC_F_WRITE, 0);

        // Fence to ensure descriptor writes are visible before avail ring update
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        self.controlq.push_avail(d0);

        // Fence to ensure avail ring update is visible before notify
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.mmio.notify(0);

        // Poll for completion (bounded)
        for i in 0..10_000_000u32 {
            if let Some(elem) = self.controlq.poll_used() {
                // Acknowledge interrupt (required for legacy transport)
                self.mmio.ack_interrupt();
                crate::println!(
                    "[gpu]   used: id={}, len={}, iterations={}",
                    elem.id, elem.len, i
                );
                // Don't free descriptors — use unique ones for each command
                // to rule out descriptor reuse as the corruption source
                return;
            }
            core::hint::spin_loop();
        }
        crate::println!("[gpu] WARNING: command timed out");
    }

    /// Helper: copy command to heap, send it, return the response header type field.
    fn send_typed<T>(&mut self, cmd: &T) -> u32 {
        let cmd_size = size_of::<T>();
        let resp_size = size_of::<CtrlHdr>();
        // Allocate both command and response on the heap for DMA safety
        let cmd_layout = Layout::from_size_align(cmd_size, 16).unwrap();
        let cmd_buf = unsafe { alloc::alloc::alloc(cmd_layout) };
        unsafe { core::ptr::copy_nonoverlapping(cmd as *const T as *const u8, cmd_buf, cmd_size) };

        let resp_layout = Layout::from_size_align(resp_size, 16).unwrap();
        let resp_buf = unsafe { alloc::alloc::alloc_zeroed(resp_layout) };

        self.send_cmd(
            cmd_buf,
            cmd_size as u32,
            resp_buf,
            resp_size as u32,
        );
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        let t = unsafe { core::ptr::read_volatile(resp_buf as *const u32) };
        t
    }

    // ── GPU commands ─────────────────────────────────────────────────────

    fn cmd_get_display_info(&mut self) {
        let cmd = CtrlHdr::cmd(VIRTIO_GPU_CMD_GET_DISPLAY_INFO);

        let resp_size = size_of::<RespDisplayInfo>();
        let layout = Layout::from_size_align(resp_size, 16).unwrap();
        let resp = unsafe { alloc::alloc::alloc_zeroed(layout) };

        self.send_cmd(
            &cmd as *const CtrlHdr as *const u8,
            size_of::<CtrlHdr>() as u32,
            resp,
            resp_size as u32,
        );

        let info = unsafe { &*(resp as *const RespDisplayInfo) };
        if info.hdr.type_ == VIRTIO_GPU_RESP_OK_DISPLAY_INFO {
            for i in 0..16 {
                if info.pmodes[i].enabled != 0 {
                    self.width = info.pmodes[i].r.width;
                    self.height = info.pmodes[i].r.height;
                    crate::println!(
                        "[gpu] Display {}: {}x{}",
                        i,
                        self.width,
                        self.height
                    );
                    break;
                }
            }
        } else {
            crate::println!("[gpu] GET_DISPLAY_INFO → {:#x}", info.hdr.type_);
        }
    }

    fn cmd_resource_create_2d(&mut self, id: u32, w: u32, h: u32) {
        let cmd = CmdResourceCreate2d {
            hdr: CtrlHdr::cmd(VIRTIO_GPU_CMD_RESOURCE_CREATE_2D),
            resource_id: id,
            format: VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM,
            width: w,
            height: h,
        };
        crate::println!("[gpu] CREATE_2D cmd: size={}", size_of::<CmdResourceCreate2d>());

        let cmd_size = size_of::<CmdResourceCreate2d>();
        let resp_size = size_of::<CtrlHdr>();
        let cmd_layout = Layout::from_size_align(cmd_size, 16).unwrap();
        let cmd_buf = unsafe { alloc::alloc::alloc(cmd_layout) };
        unsafe { core::ptr::copy_nonoverlapping(&cmd as *const CmdResourceCreate2d as *const u8, cmd_buf, cmd_size) };
        let resp_layout = Layout::from_size_align(resp_size, 16).unwrap();
        let resp_buf = unsafe { alloc::alloc::alloc_zeroed(resp_layout) };

        // Dump cmd bytes
        crate::print!("[gpu]   cmd bytes: ");
        for i in 0..cmd_size {
            crate::print!("{:02x} ", unsafe { *cmd_buf.add(i) });
        }
        crate::println!();

        self.send_cmd(cmd_buf, cmd_size as u32, resp_buf, resp_size as u32);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        // Dump resp bytes
        crate::print!("[gpu]   resp bytes: ");
        for i in 0..resp_size {
            crate::print!("{:02x} ", unsafe { *resp_buf.add(i) });
        }
        crate::println!();

        let t = unsafe { core::ptr::read_volatile(resp_buf as *const u32) };
        if t == VIRTIO_GPU_RESP_OK_NODATA {
            crate::println!("[gpu] Resource {} created ({}x{})", id, w, h);
        } else {
            crate::println!("[gpu] RESOURCE_CREATE_2D → {:#x}", t);
        }
    }

    fn cmd_attach_backing(&mut self, id: u32, addr: u64, length: u32) {
        let cmd = CmdAttachBacking {
            hdr: CtrlHdr::cmd(VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING),
            resource_id: id,
            nr_entries: 1,
            entry: MemEntry {
                addr,
                length,
                padding: 0,
            },
        };
        // Debug: print cmd struct size and key fields
        crate::println!("[gpu] ATTACH_BACKING cmd: size={}, res_id={}, addr={:#x}, len={}",
            size_of::<CmdAttachBacking>(), id, addr, length);

        let cmd_size = size_of::<CmdAttachBacking>();
        let resp_size = size_of::<CtrlHdr>();
        let cmd_layout = Layout::from_size_align(cmd_size, 16).unwrap();
        let cmd_buf = unsafe { alloc::alloc::alloc(cmd_layout) };
        unsafe { core::ptr::copy_nonoverlapping(&cmd as *const CmdAttachBacking as *const u8, cmd_buf, cmd_size) };

        let resp_layout = Layout::from_size_align(resp_size, 16).unwrap();
        let resp_buf = unsafe { alloc::alloc::alloc_zeroed(resp_layout) };

        // Debug: print first 16 bytes of cmd
        crate::print!("[gpu]   cmd bytes: ");
        for i in 0..cmd_size.min(48) {
            let b = unsafe { *cmd_buf.add(i) };
            crate::print!("{:02x} ", b);
        }
        crate::println!();

        self.send_cmd(cmd_buf, cmd_size as u32, resp_buf, resp_size as u32);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        // Debug: print response bytes
        crate::print!("[gpu]   resp bytes: ");
        for i in 0..resp_size {
            let b = unsafe { *resp_buf.add(i) };
            crate::print!("{:02x} ", b);
        }
        crate::println!();

        let t = unsafe { core::ptr::read_volatile(resp_buf as *const u32) };
        if t == VIRTIO_GPU_RESP_OK_NODATA {
            crate::println!("[gpu] Backing attached for resource {}", id);
        } else {
            crate::println!("[gpu] ATTACH_BACKING → {:#x}", t);
        }
    }

    fn cmd_set_scanout(&mut self, scanout: u32, resource_id: u32, w: u32, h: u32) {
        let cmd = CmdSetScanout {
            hdr: CtrlHdr::cmd(VIRTIO_GPU_CMD_SET_SCANOUT),
            r: Rect { x: 0, y: 0, width: w, height: h },
            scanout_id: scanout,
            resource_id,
        };
        let t = self.send_typed(&cmd);
        if t == VIRTIO_GPU_RESP_OK_NODATA {
            crate::println!("[gpu] Scanout {} → resource {}", scanout, resource_id);
        } else {
            crate::println!("[gpu] SET_SCANOUT → {:#x}", t);
        }
    }

    /// Transfer framebuffer contents to host and flush to display.
    pub fn flush(&mut self) {
        // Transfer
        let cmd1 = CmdTransferToHost2d {
            hdr: CtrlHdr::cmd(VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D),
            r: Rect { x: 0, y: 0, width: self.width, height: self.height },
            offset: 0,
            resource_id: 1,
            padding: 0,
        };
        let t = self.send_typed(&cmd1);
        if t != VIRTIO_GPU_RESP_OK_NODATA {
            crate::println!("[gpu] TRANSFER → {:#x}", t);
        }

        // Flush
        let cmd2 = CmdResourceFlush {
            hdr: CtrlHdr::cmd(VIRTIO_GPU_CMD_RESOURCE_FLUSH),
            r: Rect { x: 0, y: 0, width: self.width, height: self.height },
            resource_id: 1,
            padding: 0,
        };
        let t = self.send_typed(&cmd2);
        if t != VIRTIO_GPU_RESP_OK_NODATA {
            crate::println!("[gpu] FLUSH → {:#x}", t);
        }
    }
}
