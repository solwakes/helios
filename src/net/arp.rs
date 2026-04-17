/// ARP (Address Resolution Protocol) over Ethernet+IPv4.
///
/// Packet format (28 bytes):
///   u16 hw_type       = 1 (Ethernet)
///   u16 proto_type    = 0x0800 (IPv4)
///   u8  hw_len        = 6
///   u8  proto_len     = 4
///   u16 opcode        = 1 (request) or 2 (reply)
///   u8[6] sha         (sender hardware address)
///   u8[4] spa         (sender protocol address)
///   u8[6] tha         (target hardware address)
///   u8[4] tpa         (target protocol address)

use super::{eth, OUR_IP};

pub const ARP_PACKET_SIZE: usize = 28;
pub const OP_REQUEST: u16 = 1;
pub const OP_REPLY: u16 = 2;
pub const HW_ETHERNET: u16 = 1;

/// Handle an incoming ARP packet (wrapped in an Ethernet frame).
pub fn handle(frame: &[u8]) {
    if frame.len() < eth::ETH_HEADER_SIZE + ARP_PACKET_SIZE {
        return;
    }
    let pkt = &frame[eth::ETH_HEADER_SIZE..];

    let hw_type = u16::from_be_bytes([pkt[0], pkt[1]]);
    let proto_type = u16::from_be_bytes([pkt[2], pkt[3]]);
    let hw_len = pkt[4];
    let proto_len = pkt[5];
    let opcode = u16::from_be_bytes([pkt[6], pkt[7]]);

    if hw_type != HW_ETHERNET
        || proto_type != eth::ETHERTYPE_IPV4
        || hw_len != 6
        || proto_len != 4
    {
        return;
    }

    let mut sha = [0u8; 6];
    sha.copy_from_slice(&pkt[8..14]);
    let mut spa = [0u8; 4];
    spa.copy_from_slice(&pkt[14..18]);
    let mut _tha = [0u8; 6];
    _tha.copy_from_slice(&pkt[18..24]);
    let mut tpa = [0u8; 4];
    tpa.copy_from_slice(&pkt[24..28]);

    // Cache the sender's MAC regardless of opcode.
    super::arp_insert(spa, sha);

    unsafe { super::STATS.arp_rx += 1; }

    match opcode {
        OP_REQUEST => {
            // If it's for our IP, send a reply.
            if tpa == OUR_IP {
                send_reply(sha, spa);
            }
        }
        OP_REPLY => {
            // Was this the reply we were waiting for?
            let st = super::stats_mut();
            if st.arp_reply_mac.is_none() && spa == st.arp_pending_ip {
                st.arp_reply_mac = Some(sha);
            }
        }
        _ => {}
    }
}

/// Send an ARP reply.
fn send_reply(target_mac: [u8; 6], target_ip: [u8; 4]) {
    let our_mac = super::our_mac();
    let mut pkt = [0u8; ARP_PACKET_SIZE];
    // hw_type = 1
    pkt[0..2].copy_from_slice(&HW_ETHERNET.to_be_bytes());
    // proto_type = 0x0800
    pkt[2..4].copy_from_slice(&eth::ETHERTYPE_IPV4.to_be_bytes());
    pkt[4] = 6;
    pkt[5] = 4;
    pkt[6..8].copy_from_slice(&OP_REPLY.to_be_bytes());
    // sha = our mac
    pkt[8..14].copy_from_slice(&our_mac);
    // spa = our ip
    pkt[14..18].copy_from_slice(&OUR_IP);
    // tha = target's mac
    pkt[18..24].copy_from_slice(&target_mac);
    // tpa = target's ip
    pkt[24..28].copy_from_slice(&target_ip);

    unsafe { super::STATS.arp_tx += 1; }
    super::send_eth(target_mac, eth::ETHERTYPE_ARP, &pkt);
}

/// Send an ARP request (broadcast) asking for the MAC of `target_ip`.
pub fn send_request(target_ip: [u8; 4]) {
    let our_mac = super::our_mac();
    let mut pkt = [0u8; ARP_PACKET_SIZE];
    pkt[0..2].copy_from_slice(&HW_ETHERNET.to_be_bytes());
    pkt[2..4].copy_from_slice(&eth::ETHERTYPE_IPV4.to_be_bytes());
    pkt[4] = 6;
    pkt[5] = 4;
    pkt[6..8].copy_from_slice(&OP_REQUEST.to_be_bytes());
    pkt[8..14].copy_from_slice(&our_mac);
    pkt[14..18].copy_from_slice(&OUR_IP);
    // tha = all zeros (unknown)
    // pkt[18..24] = 0
    pkt[24..28].copy_from_slice(&target_ip);

    let st = super::stats_mut();
    st.arp_pending_ip = target_ip;
    st.arp_reply_mac = None;
    st.arp_tx += 1;
    super::send_eth(eth::BROADCAST_MAC, eth::ETHERTYPE_ARP, &pkt);
}
