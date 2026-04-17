/// Network protocol stack for Helios.
/// Implements minimal Ethernet + ARP + IPv4 + ICMP echo (ping).
///
/// Static config:
///   IP:      10.0.2.15
///   Gateway: 10.0.2.2
///   Netmask: 255.255.255.0

pub mod arp;
pub mod eth;
pub mod icmp;
pub mod ip;
pub mod tcp;

use crate::virtio::net as vnet;

/// Our static IP address (the QEMU user-mode default guest IP).
pub const OUR_IP: [u8; 4] = [10, 0, 2, 15];
/// Gateway address.
pub const GATEWAY_IP: [u8; 4] = [10, 0, 2, 2];
/// Netmask.
pub const NETMASK: [u8; 4] = [255, 255, 255, 0];

/// Stats — exposed for the graph node.
pub struct NetStats {
    pub rx_frames: u64,
    pub tx_frames: u64,
    pub arp_rx: u64,
    pub arp_tx: u64,
    pub icmp_rx: u64,
    pub icmp_tx: u64,
    /// Pending ping state (only one ping outstanding at a time).
    pub ping_outstanding: bool,
    pub ping_seq: u16,
    pub ping_ident: u16,
    pub ping_target_ip: [u8; 4],
    /// Set when the matching ICMP echo reply comes back (value in microseconds).
    pub ping_reply_us: Option<u64>,
    /// Pending ARP state.
    pub arp_pending_ip: [u8; 4],
    pub arp_reply_mac: Option<[u8; 6]>,
}

impl NetStats {
    const fn new() -> Self {
        NetStats {
            rx_frames: 0,
            tx_frames: 0,
            arp_rx: 0,
            arp_tx: 0,
            icmp_rx: 0,
            icmp_tx: 0,
            ping_outstanding: false,
            ping_seq: 0,
            ping_ident: 0xB001,
            ping_target_ip: [0; 4],
            ping_reply_us: None,
            arp_pending_ip: [0; 4],
            arp_reply_mac: None,
        }
    }
}

static mut STATS: NetStats = NetStats::new();

#[allow(static_mut_refs)]
pub fn stats() -> &'static NetStats {
    unsafe { &STATS }
}

#[allow(static_mut_refs)]
pub fn stats_mut() -> &'static mut NetStats {
    unsafe { &mut STATS }
}

/// ARP cache: small fixed-size table mapping IPv4 → MAC.
pub const ARP_CACHE_SIZE: usize = 8;

pub struct ArpCacheEntry {
    pub ip: [u8; 4],
    pub mac: [u8; 6],
    pub valid: bool,
}

pub static mut ARP_CACHE: [ArpCacheEntry; ARP_CACHE_SIZE] = [
    ArpCacheEntry { ip: [0; 4], mac: [0; 6], valid: false },
    ArpCacheEntry { ip: [0; 4], mac: [0; 6], valid: false },
    ArpCacheEntry { ip: [0; 4], mac: [0; 6], valid: false },
    ArpCacheEntry { ip: [0; 4], mac: [0; 6], valid: false },
    ArpCacheEntry { ip: [0; 4], mac: [0; 6], valid: false },
    ArpCacheEntry { ip: [0; 4], mac: [0; 6], valid: false },
    ArpCacheEntry { ip: [0; 4], mac: [0; 6], valid: false },
    ArpCacheEntry { ip: [0; 4], mac: [0; 6], valid: false },
];

#[allow(static_mut_refs)]
pub fn arp_lookup(ip: &[u8; 4]) -> Option<[u8; 6]> {
    unsafe {
        for e in ARP_CACHE.iter() {
            if e.valid && &e.ip == ip {
                return Some(e.mac);
            }
        }
    }
    None
}

#[allow(static_mut_refs)]
pub fn arp_insert(ip: [u8; 4], mac: [u8; 6]) {
    unsafe {
        // Find existing entry or empty slot
        for e in ARP_CACHE.iter_mut() {
            if e.valid && e.ip == ip {
                e.mac = mac;
                return;
            }
        }
        for e in ARP_CACHE.iter_mut() {
            if !e.valid {
                e.ip = ip;
                e.mac = mac;
                e.valid = true;
                return;
            }
        }
        // All full; overwrite slot 0.
        ARP_CACHE[0].ip = ip;
        ARP_CACHE[0].mac = mac;
        ARP_CACHE[0].valid = true;
    }
}

/// Our MAC address (cached after init).
pub fn our_mac() -> [u8; 6] {
    vnet::mac().unwrap_or([0; 6])
}

/// Handle an incoming Ethernet frame.
pub fn handle_frame(frame: &[u8]) {
    unsafe { STATS.rx_frames += 1; }

    if frame.len() < eth::ETH_HEADER_SIZE {
        return;
    }

    // Parse ethertype (bytes 12..14)
    let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
    match ethertype {
        eth::ETHERTYPE_ARP => arp::handle(frame),
        eth::ETHERTYPE_IPV4 => ip::handle(frame),
        _ => {
            // Unknown; drop silently.
        }
    }
}

/// Send an Ethernet frame. Fills in src MAC.
pub fn send_eth(dst_mac: [u8; 6], ethertype: u16, payload: &[u8]) -> bool {
    if payload.len() > 1500 {
        return false;
    }
    let our = our_mac();
    let total = eth::ETH_HEADER_SIZE + payload.len();
    // Pad to minimum 60-byte Ethernet payload (64 - 4 CRC)
    let total = total.max(60);

    let mut buf = [0u8; 1600];
    // Dst MAC
    buf[0..6].copy_from_slice(&dst_mac);
    // Src MAC
    buf[6..12].copy_from_slice(&our);
    // EtherType
    let et = ethertype.to_be_bytes();
    buf[12] = et[0];
    buf[13] = et[1];
    // Payload
    buf[14..14 + payload.len()].copy_from_slice(payload);

    unsafe { STATS.tx_frames += 1; }
    vnet::send_frame(&buf[..total])
}

/// The graph node ID for net0 (assigned by register_graph_node).
static mut NET0_NODE_ID: u64 = 0;

#[allow(static_mut_refs)]
pub fn net0_node_id() -> u64 {
    unsafe { NET0_NODE_ID }
}

/// Register a net0 node under /devices in the graph.
pub fn register_graph_node() {
    if !vnet::is_present() {
        return;
    }
    let g = crate::graph::get_mut();
    // devices node is ID 3 (see graph::init::bootstrap)
    let devices_id = 3;
    let net_id = g.create_node(crate::graph::NodeType::System, "net0");
    g.add_edge(devices_id, "child", net_id);
    unsafe { NET0_NODE_ID = net_id; }
    update_graph_node();
}

/// Refresh the net0 node content with current stats.
pub fn update_graph_node() {
    let id = net0_node_id();
    if id == 0 {
        return;
    }
    let mac = our_mac();
    let s = stats();
    let info = alloc::format!(
        "VirtIO net device\n\
         MAC: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}\n\
         IP: {}.{}.{}.{}\n\
         Gateway: {}.{}.{}.{}\n\
         Netmask: {}.{}.{}.{}\n\
         RX frames: {}\n\
         TX frames: {}\n\
         ARP rx/tx: {}/{}\n\
         ICMP rx/tx: {}/{}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5],
        OUR_IP[0], OUR_IP[1], OUR_IP[2], OUR_IP[3],
        GATEWAY_IP[0], GATEWAY_IP[1], GATEWAY_IP[2], GATEWAY_IP[3],
        NETMASK[0], NETMASK[1], NETMASK[2], NETMASK[3],
        s.rx_frames, s.tx_frames,
        s.arp_rx, s.arp_tx,
        s.icmp_rx, s.icmp_tx
    );
    let g = crate::graph::get_mut();
    if let Some(node) = g.get_node_mut(id) {
        node.content = info.into_bytes();
    }
}
