/// Minimal ICMP: echo request/reply (ping).
///
/// ICMP header (8 bytes):
///   u8  type        (8 = echo request, 0 = echo reply)
///   u8  code
///   u16 checksum
///   u16 identifier  (for echo)
///   u16 sequence    (for echo)
///   ... payload

use super::ip;

pub const ICMP_ECHO_REQUEST: u8 = 8;
pub const ICMP_ECHO_REPLY: u8 = 0;

const ICMP_HDR_SIZE: usize = 8;

/// Handle an incoming ICMP packet.
pub fn handle(src_ip: [u8; 4], src_mac: [u8; 6], payload: &[u8]) {
    if payload.len() < ICMP_HDR_SIZE {
        return;
    }
    let type_ = payload[0];
    let _code = payload[1];

    unsafe { super::STATS.icmp_rx += 1; }

    match type_ {
        ICMP_ECHO_REQUEST => {
            send_echo_reply(src_ip, src_mac, payload);
        }
        ICMP_ECHO_REPLY => {
            // If we have an outstanding ping, record the time.
            let st = super::stats_mut();
            if st.ping_outstanding {
                let ident = u16::from_be_bytes([payload[4], payload[5]]);
                let seq = u16::from_be_bytes([payload[6], payload[7]]);
                if ident == st.ping_ident && seq == st.ping_seq && src_ip == st.ping_target_ip {
                    // Extract the timestamp we embedded in the payload.
                    if payload.len() >= ICMP_HDR_SIZE + 8 {
                        let sent_us = u64::from_le_bytes([
                            payload[8], payload[9], payload[10], payload[11],
                            payload[12], payload[13], payload[14], payload[15],
                        ]);
                        let now_us = now_micros();
                        let elapsed_us = now_us.saturating_sub(sent_us);
                        st.ping_reply_us = Some(elapsed_us);
                    } else {
                        st.ping_reply_us = Some(0);
                    }
                    st.ping_outstanding = false;
                }
            }
        }
        _ => {}
    }
}

fn send_echo_reply(dst_ip: [u8; 4], dst_mac: [u8; 6], req_payload: &[u8]) {
    // Build reply by copying the request and flipping type.
    if req_payload.len() > 1500 - 20 {
        return;
    }
    let mut buf = [0u8; 1480];
    buf[..req_payload.len()].copy_from_slice(req_payload);
    buf[0] = ICMP_ECHO_REPLY;
    buf[1] = 0; // code
    // Zero checksum and recompute
    buf[2] = 0;
    buf[3] = 0;
    let csum = ip::checksum(&buf[..req_payload.len()]);
    buf[2..4].copy_from_slice(&csum.to_be_bytes());

    unsafe { super::STATS.icmp_tx += 1; }
    ip::send(dst_ip, dst_mac, ip::PROTO_ICMP, &buf[..req_payload.len()]);
}

/// Send an ICMP echo request.
/// Returns true if the packet went out (doesn't wait for reply).
pub fn send_echo_request(dst_ip: [u8; 4], dst_mac: [u8; 6], ident: u16, seq: u16) -> bool {
    let mut buf = [0u8; 64];
    buf[0] = ICMP_ECHO_REQUEST;
    buf[1] = 0;
    buf[2] = 0; // checksum = 0
    buf[3] = 0;
    buf[4..6].copy_from_slice(&ident.to_be_bytes());
    buf[6..8].copy_from_slice(&seq.to_be_bytes());

    // Embed send timestamp (microseconds) in the payload.
    let now = now_micros();
    buf[8..16].copy_from_slice(&now.to_le_bytes());
    // Fill rest with a pattern.
    for i in 16..56 {
        buf[i] = (i - 8) as u8;
    }

    let total_len = 56;
    // Compute checksum over the ICMP header + payload
    let csum = ip::checksum(&buf[..total_len]);
    buf[2..4].copy_from_slice(&csum.to_be_bytes());

    unsafe { super::STATS.icmp_tx += 1; }
    ip::send(dst_ip, dst_mac, ip::PROTO_ICMP, &buf[..total_len])
}

/// Current time in microseconds.
pub fn now_micros() -> u64 {
    // RISC-V timer on QEMU virt runs at 10 MHz.
    (crate::arch::riscv64::read_time() / 10) as u64
}
