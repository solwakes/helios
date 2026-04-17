/// VirtIO network device driver for Helios.
/// Implements virtio-net with RX + TX virtqueues.
/// - Device ID: 1
/// - RX queue: 0
/// - TX queue: 1
/// - Negotiates VIRTIO_F_VERSION_1 + VIRTIO_NET_F_MAC.
/// - 12-byte virtio_net_hdr. No checksum offload, no GSO.

use super::mmio::VirtioMmio;
use super::{Virtqueue, VirtqAvail, VirtqDesc, VirtqUsed, VRING_DESC_F_WRITE};
use alloc::alloc::alloc_zeroed;
use core::alloc::Layout;
use core::ptr;

// ── Feature bits (low 32) ────────────────────────────────────────────────────
#[allow(dead_code)]
const VIRTIO_NET_F_CSUM: u32 = 1 << 0;
#[allow(dead_code)]
const VIRTIO_NET_F_GUEST_CSUM: u32 = 1 << 1;
const VIRTIO_NET_F_MAC: u32 = 1 << 5;
#[allow(dead_code)]
const VIRTIO_NET_F_STATUS: u32 = 1 << 16;
#[allow(dead_code)]
const VIRTIO_NET_F_MRG_RXBUF: u32 = 1 << 15;

// ── Feature bits (high 32, shifted down) ─────────────────────────────────────
const VIRTIO_F_VERSION_1: u32 = 1 << (32 - 32); // bit 32

// ── virtio_net_hdr (12 bytes when VIRTIO_F_VERSION_1 negotiated) ─────────────
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VirtioNetHdr {
    pub flags: u8,
    pub gso_type: u8,
    pub hdr_len: u16,
    pub gso_size: u16,
    pub csum_start: u16,
    pub csum_offset: u16,
    pub num_buffers: u16,
}

pub const NET_HDR_SIZE: usize = 12;
pub const RX_BUF_SIZE: usize = 2048; // header + 1500 MTU + some slack

/// Number of pre-allocated RX and TX buffers.
pub const RX_BUF_COUNT: usize = 16;
pub const TX_BUF_COUNT: usize = 16;

pub struct VirtioNet {
    mmio: VirtioMmio,
    rxq: Virtqueue,
    txq: Virtqueue,
    /// Pre-allocated RX buffers, one per descriptor slot.
    rx_bufs: *mut u8,
    /// Pre-allocated TX buffers, one per descriptor slot (not strictly needed
    /// since we free between sends, but keeps lifetime simple).
    tx_bufs: *mut u8,
    /// MAC address
    pub mac: [u8; 6],
    /// Packet counters
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

static mut NET_DEV: Option<VirtioNet> = None;

/// Initialize the global net device. Returns true if a device was found.
pub fn init() -> bool {
    match VirtioNet::init() {
        Some(n) => {
            crate::println!(
                "[net] VirtIO net initialized, MAC={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                n.mac[0], n.mac[1], n.mac[2], n.mac[3], n.mac[4], n.mac[5]
            );
            unsafe { NET_DEV = Some(n); }
            true
        }
        None => {
            crate::println!("[net] No VirtIO net device found");
            false
        }
    }
}

#[allow(static_mut_refs)]
pub fn get_mut() -> Option<&'static mut VirtioNet> {
    unsafe { NET_DEV.as_mut() }
}

#[allow(static_mut_refs)]
pub fn is_present() -> bool {
    unsafe { NET_DEV.is_some() }
}

/// Poll for received packets, dispatching each to the net stack.
#[allow(static_mut_refs)]
pub fn poll() -> usize {
    let dev = match unsafe { NET_DEV.as_mut() } {
        Some(d) => d,
        None => return 0,
    };
    dev.poll_rx()
}

/// Send an Ethernet frame. Returns true on success.
#[allow(static_mut_refs)]
pub fn send_frame(frame: &[u8]) -> bool {
    let dev = match unsafe { NET_DEV.as_mut() } {
        Some(d) => d,
        None => return false,
    };
    dev.tx_frame(frame)
}

/// Get MAC address.
#[allow(static_mut_refs)]
pub fn mac() -> Option<[u8; 6]> {
    unsafe { NET_DEV.as_ref().map(|d| d.mac) }
}

impl VirtioNet {
    pub fn init() -> Option<Self> {
        let mmio = VirtioMmio::probe(1)?; // device ID 1 = network
        crate::println!(
            "[net] Found net device @ {:#x} (version {})",
            mmio.base, mmio.version
        );

        // Negotiate VIRTIO_F_VERSION_1 (hi bit 0) + VIRTIO_NET_F_MAC (lo bit 5).
        let lo = VIRTIO_NET_F_MAC;
        let hi = VIRTIO_F_VERSION_1;
        if !mmio.init_device_with_features(lo, hi) {
            crate::println!("[net] Failed to negotiate features");
            return None;
        }

        // Read MAC from config space (if VIRTIO_NET_F_MAC was accepted).
        let mut mac = [0u8; 6];
        for i in 0..6 {
            mac[i] = mmio.read_config_u8(i);
        }

        // Set up RX queue (0) and TX queue (1)
        let (dp0, ap0, up0, qs0) = mmio.setup_queue(0)?;
        let (dp1, ap1, up1, qs1) = mmio.setup_queue(1)?;

        let rxq = Virtqueue::new(
            dp0 as *mut VirtqDesc,
            ap0 as *mut VirtqAvail,
            up0 as *mut VirtqUsed,
            qs0,
        );
        let txq = Virtqueue::new(
            dp1 as *mut VirtqDesc,
            ap1 as *mut VirtqAvail,
            up1 as *mut VirtqUsed,
            qs1,
        );

        // Allocate RX and TX buffer pools (one buffer per slot).
        let rx_count = (qs0 as usize).min(RX_BUF_COUNT);
        let tx_count = (qs1 as usize).min(TX_BUF_COUNT);

        let rx_layout = Layout::from_size_align(rx_count * RX_BUF_SIZE, 16).unwrap();
        let rx_bufs = unsafe { alloc_zeroed(rx_layout) };
        if rx_bufs.is_null() {
            crate::println!("[net] Failed to allocate RX buffers");
            return None;
        }

        let tx_layout = Layout::from_size_align(tx_count * RX_BUF_SIZE, 16).unwrap();
        let tx_bufs = unsafe { alloc_zeroed(tx_layout) };
        if tx_bufs.is_null() {
            crate::println!("[net] Failed to allocate TX buffers");
            return None;
        }

        let mut dev = VirtioNet {
            mmio,
            rxq,
            txq,
            rx_bufs,
            tx_bufs,
            mac,
            rx_packets: 0,
            tx_packets: 0,
            rx_bytes: 0,
            tx_bytes: 0,
        };

        // Pre-post RX buffers.
        for i in 0..rx_count {
            dev.enqueue_rx_buf(i);
        }

        // Notify the device of available RX buffers.
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        unsafe { core::arch::asm!("fence iorw, iorw"); }
        dev.mmio.notify(0);

        dev.mmio.driver_ok();
        crate::println!(
            "[net] Device ready, rxq size={}, txq size={}, bufs={}/{}",
            qs0, qs1, rx_count, tx_count
        );

        Some(dev)
    }

    fn enqueue_rx_buf(&mut self, i: usize) {
        let desc_idx = match self.rxq.alloc_desc() {
            Some(d) => d,
            None => return,
        };
        let addr = unsafe { self.rx_bufs.add(i * RX_BUF_SIZE) } as u64;
        // Single device-writable descriptor that will receive header + frame.
        self.rxq.set_desc(desc_idx, addr, RX_BUF_SIZE as u32, VRING_DESC_F_WRITE, 0);
        self.rxq.push_avail(desc_idx);
    }

    /// Poll RX used ring. Returns number of frames processed.
    fn poll_rx(&mut self) -> usize {
        let mut count = 0;

        while let Some(elem) = self.rxq.poll_used() {
            self.mmio.ack_interrupt();

            let desc_idx = elem.id as u16;

            // Read the buffer address and written length.
            let buf_addr = unsafe {
                let d = self.rxq.desc.add(desc_idx as usize);
                ptr::read_volatile(&(*d).addr)
            };
            let total_len = elem.len as usize;

            // Only process if length > 12 (header must be present).
            if total_len > NET_HDR_SIZE {
                let payload_addr = buf_addr + NET_HDR_SIZE as u64;
                let payload_len = total_len - NET_HDR_SIZE;
                // Safety: the device wrote at most RX_BUF_SIZE bytes into the buffer.
                let frame = unsafe {
                    core::slice::from_raw_parts(payload_addr as *const u8, payload_len)
                };
                self.rx_packets += 1;
                self.rx_bytes += payload_len as u64;
                // Dispatch to net stack. We copy into a temp buffer so the caller
                // can keep using the data across requeue (which we do right after).
                let mut stack_buf = [0u8; RX_BUF_SIZE];
                let n = payload_len.min(stack_buf.len());
                stack_buf[..n].copy_from_slice(&frame[..n]);
                // Figure out which buffer index this was before requeuing.
                let base = self.rx_bufs as u64;
                let buf_idx = ((buf_addr - base) as usize) / RX_BUF_SIZE;

                // Requeue the buffer.
                self.rxq.free_desc(desc_idx);
                self.enqueue_rx_buf(buf_idx);

                // Dispatch — must be AFTER requeue so the net stack can respond
                // with tx_frame without being blocked.
                crate::net::handle_frame(&stack_buf[..n]);
                count += 1;
            } else {
                // Unknown short frame — just requeue.
                let base = self.rx_bufs as u64;
                let buf_idx = ((buf_addr - base) as usize) / RX_BUF_SIZE;
                self.rxq.free_desc(desc_idx);
                self.enqueue_rx_buf(buf_idx);
            }
        }

        if count > 0 {
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            unsafe { core::arch::asm!("fence iorw, iorw"); }
            self.mmio.notify(0);
        }

        count
    }

    /// Transmit an Ethernet frame.
    fn tx_frame(&mut self, frame: &[u8]) -> bool {
        // Reap any completed TX descriptors first.
        while let Some(_elem) = self.txq.poll_used() {
            self.mmio.ack_interrupt();
            // We use per-send descriptor allocation; we can't easily map desc_idx back
            // to a free since we already freed on send. So this is a no-op here — we
            // instead free inside the allocation path.
            // (We free at end of function below.)
        }

        if frame.len() > RX_BUF_SIZE - NET_HDR_SIZE {
            return false;
        }

        // Grab a TX buffer slot. We use the descriptor index as the buffer index.
        let d0 = match self.txq.alloc_desc() { Some(d) => d, None => return false };
        // We need 2 descriptors chained (header + payload) because the header is
        // a distinct memory region semantically; but we can put them contiguously
        // in the same buffer. Simpler: write header + frame into one buffer, use
        // one descriptor.
        let buf_idx = d0 as usize % TX_BUF_COUNT;
        let buf = unsafe { self.tx_bufs.add(buf_idx * RX_BUF_SIZE) };

        // Zero the header.
        unsafe {
            ptr::write_bytes(buf, 0, NET_HDR_SIZE);
            core::ptr::copy_nonoverlapping(frame.as_ptr(), buf.add(NET_HDR_SIZE), frame.len());
        }

        let total_len = (NET_HDR_SIZE + frame.len()) as u32;
        // Device-readable descriptor.
        self.txq.set_desc(d0, buf as u64, total_len, 0, 0);

        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.txq.push_avail(d0);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        unsafe { core::arch::asm!("fence iorw, iorw"); }

        self.mmio.notify(1);

        self.tx_packets += 1;
        self.tx_bytes += frame.len() as u64;

        // Poll for completion so we can free the descriptor.
        for _ in 0..1_000_000u32 {
            if let Some(elem) = self.txq.poll_used() {
                self.mmio.ack_interrupt();
                self.txq.free_desc(elem.id as u16);
                return true;
            }
            core::hint::spin_loop();
        }
        // Timed out; free anyway so we don't leak descriptors.
        self.txq.free_desc(d0);
        false
    }
}
