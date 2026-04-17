/// Minimal IPv4 handling — just enough to route ICMP echo.

use super::{eth, icmp, tcp, OUR_IP};

pub const IPV4_MIN_HEADER_SIZE: usize = 20;
pub const PROTO_ICMP: u8 = 1;
pub const PROTO_TCP: u8 = 6;
#[allow(dead_code)]
pub const PROTO_UDP: u8 = 17;

/// Compute the standard one's-complement IPv4 checksum over `buf`.
pub fn checksum(buf: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < buf.len() {
        sum += u16::from_be_bytes([buf[i], buf[i + 1]]) as u32;
        i += 2;
    }
    if i < buf.len() {
        sum += (buf[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// Handle an incoming IPv4 packet (in an Ethernet frame).
pub fn handle(frame: &[u8]) {
    if frame.len() < eth::ETH_HEADER_SIZE + IPV4_MIN_HEADER_SIZE {
        return;
    }
    let ip = &frame[eth::ETH_HEADER_SIZE..];
    let version_ihl = ip[0];
    let version = version_ihl >> 4;
    let ihl = (version_ihl & 0x0f) as usize * 4;
    if version != 4 || ihl < IPV4_MIN_HEADER_SIZE {
        return;
    }
    let total_length = u16::from_be_bytes([ip[2], ip[3]]) as usize;
    if ip.len() < total_length || total_length < ihl {
        return;
    }

    let proto = ip[9];
    let mut src_ip = [0u8; 4];
    src_ip.copy_from_slice(&ip[12..16]);
    let mut dst_ip = [0u8; 4];
    dst_ip.copy_from_slice(&ip[16..20]);

    // Accept packets addressed to us or broadcast.
    if dst_ip != OUR_IP && dst_ip != [255, 255, 255, 255] {
        return;
    }

    // Extract L4 payload.
    let payload = &ip[ihl..total_length];

    // Pull src MAC from the ethernet frame and cache ARP mapping (opportunistic).
    let mut src_mac = [0u8; 6];
    src_mac.copy_from_slice(&frame[6..12]);
    super::arp_insert(src_ip, src_mac);

    match proto {
        PROTO_ICMP => icmp::handle(src_ip, src_mac, payload),
        PROTO_TCP => tcp::handle(src_ip, src_mac, dst_ip, payload),
        _ => {}
    }
}

/// Build an IPv4 packet with the given `protocol` and `payload` and send it out.
pub fn send(dst_ip: [u8; 4], dst_mac: [u8; 6], protocol: u8, payload: &[u8]) -> bool {
    if payload.len() > 1500 - IPV4_MIN_HEADER_SIZE {
        return false;
    }
    let mut pkt = [0u8; 1500];
    let total_len = IPV4_MIN_HEADER_SIZE + payload.len();
    // Version=4, IHL=5
    pkt[0] = (4 << 4) | 5;
    pkt[1] = 0; // DSCP/ECN
    // Total length
    pkt[2..4].copy_from_slice(&(total_len as u16).to_be_bytes());
    // Identification
    pkt[4..6].copy_from_slice(&0u16.to_be_bytes());
    // Flags + fragment offset
    pkt[6..8].copy_from_slice(&0u16.to_be_bytes());
    // TTL
    pkt[8] = 64;
    // Protocol
    pkt[9] = protocol;
    // Header checksum = 0 for now
    pkt[10] = 0;
    pkt[11] = 0;
    // Src IP
    pkt[12..16].copy_from_slice(&OUR_IP);
    // Dst IP
    pkt[16..20].copy_from_slice(&dst_ip);
    // Compute header checksum
    let csum = checksum(&pkt[..IPV4_MIN_HEADER_SIZE]);
    pkt[10..12].copy_from_slice(&csum.to_be_bytes());
    // Payload
    pkt[IPV4_MIN_HEADER_SIZE..total_len].copy_from_slice(payload);

    super::send_eth(dst_mac, eth::ETHERTYPE_IPV4, &pkt[..total_len])
}
