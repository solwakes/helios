/// TCP implementation for Helios — minimal RFC 793 + 1122 subset.
///
/// States: CLOSED, LISTEN, SYN_SENT, SYN_RECEIVED, ESTABLISHED,
///         FIN_WAIT_1, FIN_WAIT_2, CLOSE_WAIT, LAST_ACK.
/// (We collapse TIME_WAIT → CLOSED for simplicity.)
///
/// No congestion control, no Nagle, no URG, no SACK, no window scaling,
/// no timestamps, MSS = 1460 fixed, fixed receive window = 8192.
///
/// Single-threaded polling design — all state is `static mut`, gated behind
/// `unsafe` blocks in tune with the rest of the Helios kernel.
///
/// Wire format reminder (TCP header, 20 bytes min):
///   u16 src_port
///   u16 dst_port
///   u32 seq
///   u32 ack
///   u8  data_offset<<4 | reserved (3 bits, zero) | ns(1 bit, zero) — actually:
///       upper 4 bits = data offset in 32-bit words
///       lower 4 bits = reserved/NS
///   u8  flags (CWR ECE URG ACK PSH RST SYN FIN)
///   u16 window
///   u16 checksum  ← includes IPv4 pseudo-header
///   u16 urgent_ptr
///   ... options ... ... data ...

use super::{ip, OUR_IP};

use alloc::collections::VecDeque;
use alloc::format;
use alloc::vec::Vec;

// ── Constants ────────────────────────────────────────────────────────────────

pub const TCP_HDR_MIN: usize = 20;

pub const FLAG_FIN: u8 = 0x01;
pub const FLAG_SYN: u8 = 0x02;
pub const FLAG_RST: u8 = 0x04;
pub const FLAG_PSH: u8 = 0x08;
pub const FLAG_ACK: u8 = 0x10;

pub const MSS: u16 = 1460;
pub const DEFAULT_RCV_WND: u16 = 8192;

pub const MAX_SOCKETS: usize = 16;
pub const MAX_LISTENERS: usize = 8;
pub const MAX_REASSEMBLE: usize = 4;

/// 1 second in microseconds.
pub const RETRANS_TIMEOUT_US: u64 = 1_000_000;
pub const MAX_RETRIES: u8 = 3;

// ── Time source (microseconds) ───────────────────────────────────────────────

pub fn now_us() -> u64 {
    // QEMU virt timer is 10 MHz → divide by 10 for microseconds.
    (crate::arch::riscv64::read_time() / 10) as u64
}

// ── State enum ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Closed,
    Listen,
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    LastAck,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Closed => "CLOSED",
            State::Listen => "LISTEN",
            State::SynSent => "SYN_SENT",
            State::SynReceived => "SYN_RECEIVED",
            State::Established => "ESTABLISHED",
            State::FinWait1 => "FIN_WAIT_1",
            State::FinWait2 => "FIN_WAIT_2",
            State::CloseWait => "CLOSE_WAIT",
            State::LastAck => "LAST_ACK",
        }
    }
}

// ── Socket ───────────────────────────────────────────────────────────────────

pub struct TcpSocket {
    pub local_port: u16,
    pub remote_ip: [u8; 4],
    pub remote_port: u16,
    pub peer_mac: [u8; 6],
    pub state: State,

    /// Our initial send seq number.
    pub iss: u32,
    /// Oldest unacknowledged sequence number.
    pub snd_una: u32,
    /// Peer's advertised receive window.
    pub snd_wnd: u16,

    /// Peer's initial sequence number.
    pub irs: u32,
    /// Next sequence number we expect to receive.
    pub rcv_nxt: u32,

    /// Outgoing bytes not yet acked. When acked they're popped off the front.
    pub send_buf: VecDeque<u8>,
    /// Application-readable received data (in-order).
    pub recv_buf: VecDeque<u8>,
    /// Out-of-order received segments: (seq, bytes).
    pub reassemble: Vec<(u32, Vec<u8>)>,

    /// Is there an outgoing FIN pending (i.e. user called close())?
    pub fin_pending: bool,
    /// Have we already put the FIN on the wire?
    pub fin_sent: bool,

    /// Retransmit bookkeeping.
    pub last_send_us: u64,
    pub retries: u8,

    /// Whether there's an ACK queued to flush (delayed ACK).
    /// (We actually ACK immediately on data, but keep this for future.)
    pub ack_pending: bool,

    /// Accept flag: this socket transitioned from SYN_RECEIVED → ESTABLISHED
    /// but hasn't been handed to the application yet.
    pub accepted: bool,

    /// Whether the listener that spawned us has claimed this socket via accept().
    /// (Until accepted, the socket exists but the app doesn't see it yet.)
    pub handed_out: bool,

    pub rx_bytes: u64,
    pub tx_bytes: u64,

    /// Graph node ID (0 = not registered).
    pub node_id: u64,
}

// ── Listener ─────────────────────────────────────────────────────────────────

pub struct Listener {
    pub port: u16,
    /// FIFO of socket indices waiting to be accept()-ed.
    pub backlog: VecDeque<usize>,
    pub node_id: u64,
}

// ── Global tables ────────────────────────────────────────────────────────────

const NONE_SOCK: Option<TcpSocket> = None;
const NONE_LST: Option<Listener> = None;

static mut SOCKETS: [Option<TcpSocket>; MAX_SOCKETS] = [NONE_SOCK; MAX_SOCKETS];
static mut LISTENERS: [Option<Listener>; MAX_LISTENERS] = [NONE_LST; MAX_LISTENERS];

pub struct TcpStats {
    pub rx_segments: u64,
    pub tx_segments: u64,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub resets_rx: u64,
    pub resets_tx: u64,
    pub retransmits: u64,
    pub accepts: u64,
    pub closes: u64,
}

static mut TCP_STATS: TcpStats = TcpStats {
    rx_segments: 0,
    tx_segments: 0,
    rx_bytes: 0,
    tx_bytes: 0,
    resets_rx: 0,
    resets_tx: 0,
    retransmits: 0,
    accepts: 0,
    closes: 0,
};

#[allow(static_mut_refs)]
pub fn stats() -> &'static TcpStats {
    unsafe { &TCP_STATS }
}

// ── Public API ───────────────────────────────────────────────────────────────

pub type ListenerHandle = usize;
pub type SocketHandle = usize;

/// Register a listener on the given port. Returns a handle on success.
/// Returns None if there's already a listener or no free listener slot.
#[allow(static_mut_refs)]
pub fn tcp_listen(port: u16) -> Option<ListenerHandle> {
    unsafe {
        // Check for existing listener on this port.
        for slot in LISTENERS.iter() {
            if let Some(l) = slot {
                if l.port == port {
                    return None;
                }
            }
        }
        // Find empty slot.
        for (i, slot) in LISTENERS.iter_mut().enumerate() {
            if slot.is_none() {
                let mut l = Listener {
                    port,
                    backlog: VecDeque::new(),
                    node_id: 0,
                };
                // Register in graph
                l.node_id = register_listener_node(port);
                *slot = Some(l);
                return Some(i);
            }
        }
        None
    }
}

/// Non-blocking accept: returns the socket index of a connection that has
/// completed the three-way handshake (i.e. reached Established at least once).
/// Also returns sockets that have since transitioned to CloseWait / Closed etc,
/// so the application can still drain received bytes.
#[allow(static_mut_refs)]
pub fn accept(handle: ListenerHandle) -> Option<SocketHandle> {
    unsafe {
        let l = LISTENERS.get_mut(handle)?.as_mut()?;
        // Pop first socket whose handshake has completed.
        let mut idx_to_return = None;
        let mut i = 0;
        while i < l.backlog.len() {
            let sock_idx = l.backlog[i];
            if let Some(sock) = SOCKETS.get_mut(sock_idx).and_then(|s| s.as_mut()) {
                let ready = !sock.handed_out && match sock.state {
                    State::Established
                    | State::CloseWait
                    | State::FinWait1
                    | State::FinWait2
                    | State::LastAck => true,
                    _ => false,
                };
                if ready {
                    sock.handed_out = true;
                    sock.accepted = true;
                    idx_to_return = Some(sock_idx);
                    l.backlog.remove(i);
                    break;
                }
            } else {
                // socket slot vanished — drop the backlog entry
                l.backlog.remove(i);
                continue;
            }
            i += 1;
        }
        if let Some(sock_idx) = idx_to_return {
            TCP_STATS.accepts += 1;
            register_socket_node(sock_idx);
        }
        idx_to_return
    }
}

/// Non-blocking receive: returns the socket's next batch of bytes, up to `buf.len()`.
/// Returns None when no bytes are available. Returns Some(0) if the peer has
/// closed (half-closed) and no bytes remain.
#[allow(static_mut_refs)]
pub fn recv(handle: SocketHandle, buf: &mut [u8]) -> Option<usize> {
    unsafe {
        let sock = SOCKETS.get_mut(handle)?.as_mut()?;
        if sock.recv_buf.is_empty() {
            // Check half-closed
            match sock.state {
                State::CloseWait | State::LastAck | State::Closed => return Some(0),
                _ => return None,
            }
        }
        let n = buf.len().min(sock.recv_buf.len());
        for i in 0..n {
            buf[i] = sock.recv_buf.pop_front().unwrap();
        }
        Some(n)
    }
}

/// Send bytes on a socket. Returns how many bytes were queued.
/// Blocks-not: simply queues and flushes one segment.
#[allow(static_mut_refs)]
pub fn send(handle: SocketHandle, data: &[u8]) -> usize {
    unsafe {
        let sock = match SOCKETS.get_mut(handle).and_then(|s| s.as_mut()) {
            Some(s) => s,
            None => return 0,
        };
        match sock.state {
            State::Established | State::CloseWait => {}
            _ => return 0,
        }
        for &b in data {
            sock.send_buf.push_back(b);
        }
        sock.tx_bytes += data.len() as u64;
        // Immediately flush a segment if there's room in the peer's window.
        transmit(handle);
        data.len()
    }
}

/// Close a socket. Initiates a graceful shutdown (sends FIN).
#[allow(static_mut_refs)]
pub fn close(handle: SocketHandle) {
    unsafe {
        let sock = match SOCKETS.get_mut(handle).and_then(|s| s.as_mut()) {
            Some(s) => s,
            None => return,
        };
        match sock.state {
            State::Established => {
                sock.fin_pending = true;
                sock.state = State::FinWait1;
                transmit(handle);
            }
            State::CloseWait => {
                sock.fin_pending = true;
                sock.state = State::LastAck;
                transmit(handle);
            }
            State::SynReceived => {
                // Abort with RST.
                send_rst(sock.remote_ip, sock.peer_mac, sock.local_port, sock.remote_port,
                         sock.snd_una, sock.rcv_nxt);
                TCP_STATS.resets_tx += 1;
                sock.state = State::Closed;
                free_socket(handle);
            }
            _ => {
                // Already closing or closed.
                free_socket(handle);
            }
        }
    }
}

/// Query a socket's state (for shell/stats commands).
#[allow(static_mut_refs)]
pub fn socket_state(handle: SocketHandle) -> Option<State> {
    unsafe {
        SOCKETS.get(handle).and_then(|s| s.as_ref()).map(|s| s.state)
    }
}

#[allow(static_mut_refs)]
pub fn socket_peer(handle: SocketHandle) -> Option<([u8; 4], u16)> {
    unsafe {
        SOCKETS.get(handle).and_then(|s| s.as_ref()).map(|s| (s.remote_ip, s.remote_port))
    }
}

// ── Active open: tcp_connect ─────────────────────────────────────────────────

/// Active-open a connection. Returns a socket handle (in SYN_SENT state).
/// Caller must poll and wait for Established.
#[allow(static_mut_refs)]
pub fn tcp_connect(dst_ip: [u8; 4], dst_port: u16) -> Option<SocketHandle> {
    // Resolve next-hop MAC.
    let same_subnet = dst_ip[0] == OUR_IP[0]
        && dst_ip[1] == OUR_IP[1]
        && dst_ip[2] == OUR_IP[2];
    let next_hop_ip = if same_subnet { dst_ip } else { super::GATEWAY_IP };

    let dst_mac = match super::arp_lookup(&next_hop_ip) {
        Some(m) => m,
        None => {
            // Emit an ARP request and wait briefly.
            super::arp::send_request(next_hop_ip);
            let start = crate::arch::riscv64::read_time();
            let deadline = start + 2 * 10_000_000usize; // 2s at 10 MHz
            let mut got = None;
            while crate::arch::riscv64::read_time() < deadline {
                crate::virtio::net::poll();
                if let Some(m) = super::arp_lookup(&next_hop_ip) {
                    got = Some(m);
                    break;
                }
                core::hint::spin_loop();
            }
            got?
        }
    };

    // Find a free socket slot.
    let sock_idx = unsafe {
        let mut found = None;
        for (i, slot) in SOCKETS.iter().enumerate() {
            if slot.is_none() {
                found = Some(i);
                break;
            }
        }
        found?
    };

    // Ephemeral local port: pick something above 49152 based on time.
    let local_port = 49152u16 + ((now_us() as u16) & 0x3FFF);
    let iss = (now_us() as u32).wrapping_mul(2654435761);

    let sock = TcpSocket {
        local_port,
        remote_ip: dst_ip,
        remote_port: dst_port,
        peer_mac: dst_mac,
        state: State::SynSent,
        iss,
        snd_una: iss,
        snd_wnd: DEFAULT_RCV_WND,
        irs: 0,
        rcv_nxt: 0,
        send_buf: VecDeque::new(),
        recv_buf: VecDeque::new(),
        reassemble: Vec::new(),
        fin_pending: false,
        fin_sent: false,
        last_send_us: 0,
        retries: 0,
        ack_pending: false,
        accepted: false,
        handed_out: true, // active open — caller already has the handle
        rx_bytes: 0,
        tx_bytes: 0,
        node_id: 0,
    };
    unsafe {
        SOCKETS[sock_idx] = Some(sock);
    }

    // Emit the SYN.
    transmit(sock_idx);
    unsafe {
        register_socket_node(sock_idx);
    }
    Some(sock_idx)
}

// ── Incoming segment dispatch (called from ip.rs) ────────────────────────────

/// Handle an incoming TCP segment.
pub fn handle(src_ip: [u8; 4], src_mac: [u8; 6], dst_ip: [u8; 4], payload: &[u8]) {
    if dst_ip != OUR_IP {
        return;
    }
    if payload.len() < TCP_HDR_MIN {
        return;
    }
    unsafe {
        TCP_STATS.rx_segments += 1;
    }

    // Checksum first.
    if !verify_checksum(src_ip, dst_ip, payload) {
        return;
    }

    let src_port = u16::from_be_bytes([payload[0], payload[1]]);
    let dst_port = u16::from_be_bytes([payload[2], payload[3]]);
    let seq = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
    let ack = u32::from_be_bytes([payload[8], payload[9], payload[10], payload[11]]);
    let data_offset = ((payload[12] >> 4) as usize) * 4;
    let flags = payload[13];
    let window = u16::from_be_bytes([payload[14], payload[15]]);

    if data_offset < TCP_HDR_MIN || data_offset > payload.len() {
        return;
    }
    let data = &payload[data_offset..];
    unsafe { TCP_STATS.rx_bytes += data.len() as u64; }

    // Try to find a matching socket by 4-tuple.
    let sock_idx = find_socket(src_ip, src_port, dst_port);

    if let Some(idx) = sock_idx {
        handle_for_socket(idx, src_mac, seq, ack, flags, window, data);
        return;
    }

    // No existing socket. Is there a listener?
    if let Some(lst_idx) = find_listener(dst_port) {
        if flags & FLAG_SYN != 0 {
            // Create a new socket in SYN_RECEIVED.
            if let Some(new_idx) = create_syn_received_socket(
                src_ip, src_mac, src_port, dst_port, seq, window,
            ) {
                unsafe {
                    if let Some(lst) = LISTENERS.get_mut(lst_idx).and_then(|s| s.as_mut()) {
                        lst.backlog.push_back(new_idx);
                    }
                }
                // Send SYN+ACK
                transmit(new_idx);
            }
            return;
        }
        // Non-SYN segment to a listener — send RST.
        if flags & FLAG_RST == 0 {
            send_rst_for_stray(src_ip, src_mac, src_port, dst_port, seq, ack, flags, data.len());
        }
        return;
    }

    // No listener: RST.
    if flags & FLAG_RST == 0 {
        send_rst_for_stray(src_ip, src_mac, src_port, dst_port, seq, ack, flags, data.len());
    }
}

fn send_rst_for_stray(
    src_ip: [u8; 4],
    src_mac: [u8; 6],
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    data_len: usize,
) {
    // Per RFC 793 §3.4: if ACK bit off, SEQ = 0, ACK = seq + len (+ syn/fin).
    // If ACK bit on, SEQ = ack.
    let (rst_seq, rst_ack, rst_flags) = if flags & FLAG_ACK != 0 {
        (ack, 0u32, FLAG_RST)
    } else {
        let mut len = data_len as u32;
        if flags & FLAG_SYN != 0 { len += 1; }
        if flags & FLAG_FIN != 0 { len += 1; }
        (0u32, seq.wrapping_add(len), FLAG_RST | FLAG_ACK)
    };
    emit_segment(src_ip, src_mac, dst_port, src_port, rst_seq, rst_ack, rst_flags, 0, &[]);
    unsafe { TCP_STATS.resets_tx += 1; }
}

// ── Per-socket handling ──────────────────────────────────────────────────────

#[allow(static_mut_refs)]
fn handle_for_socket(
    idx: usize,
    src_mac: [u8; 6],
    seq: u32,
    ack: u32,
    flags: u8,
    window: u16,
    data: &[u8],
) {
    unsafe {
        // RST handling: immediate teardown.
        if flags & FLAG_RST != 0 {
            TCP_STATS.resets_rx += 1;
            free_socket(idx);
            return;
        }

        let state = match SOCKETS.get(idx).and_then(|s| s.as_ref()) {
            Some(s) => s.state,
            None => return,
        };

        match state {
            State::SynSent => {
                // Expecting SYN+ACK.
                if (flags & FLAG_SYN) != 0 && (flags & FLAG_ACK) != 0 {
                    let s = SOCKETS[idx].as_mut().unwrap();
                    if ack != s.iss.wrapping_add(1) {
                        // Bad ACK — RST and abort.
                        send_rst(s.remote_ip, s.peer_mac, s.local_port, s.remote_port, ack, 0);
                        TCP_STATS.resets_tx += 1;
                        free_socket(idx);
                        return;
                    }
                    s.peer_mac = src_mac;
                    s.irs = seq;
                    s.rcv_nxt = seq.wrapping_add(1);
                    s.snd_una = ack;
                    s.snd_wnd = window;
                    s.state = State::Established;
                    s.retries = 0;
                    // Need to ACK
                    transmit(idx);
                    return;
                }
                // Ignore everything else in SYN_SENT.
            }
            State::SynReceived => {
                // Expecting ACK of our SYN.
                if flags & FLAG_ACK == 0 { return; }
                let s = SOCKETS[idx].as_mut().unwrap();
                if ack != s.iss.wrapping_add(1) {
                    // Bad ACK — RST.
                    send_rst(s.remote_ip, s.peer_mac, s.local_port, s.remote_port, ack, 0);
                    TCP_STATS.resets_tx += 1;
                    free_socket(idx);
                    return;
                }
                s.snd_una = ack;
                s.snd_wnd = window;
                s.state = State::Established;
                s.retries = 0;
                // Also process any data piggy-backed with the ACK.
                if !data.is_empty() || (flags & FLAG_FIN) != 0 {
                    process_segment_data(idx, seq, data, flags);
                }
            }
            State::Established
            | State::FinWait1
            | State::FinWait2
            | State::CloseWait
            | State::LastAck => {
                // Process ACK.
                if flags & FLAG_ACK != 0 {
                    process_ack(idx, ack, window);
                }
                // Process data / FIN.
                if !data.is_empty() || (flags & FLAG_FIN) != 0 {
                    process_segment_data(idx, seq, data, flags);
                }
                // Flush anything new we should send.
                let (need_tx, closed_now) = match SOCKETS.get(idx).and_then(|s| s.as_ref()) {
                    Some(s) => {
                        let need = s.ack_pending
                            || (s.send_buf.len() > 0 && s.state != State::Closed)
                            || (s.state == State::FinWait1 && s.fin_pending && !s.fin_sent)
                            || (s.state == State::LastAck && s.fin_pending && !s.fin_sent);
                        (need, s.state == State::Closed)
                    }
                    None => (false, false),
                };
                if need_tx {
                    transmit(idx);
                }
                if closed_now {
                    free_socket(idx);
                }
            }
            _ => {}
        }
    }
}

#[allow(static_mut_refs)]
fn process_ack(idx: usize, ack: u32, window: u16) {
    unsafe {
        let closed_now;
        {
            let s = match SOCKETS.get_mut(idx).and_then(|s| s.as_mut()) {
                Some(s) => s,
                None => return,
            };
            let acked = ack.wrapping_sub(s.snd_una);
            let outstanding = s.send_buf.len() as u32
                + if s.fin_sent { 1 } else { 0 };
            // Drop duplicate or spurious (beyond what we've sent) ACKs for the
            // purposes of draining send_buf, but still refresh window.
            if acked == 0 || acked > outstanding {
                s.snd_wnd = window;
                return;
            }
            let data_acked = acked.min(s.send_buf.len() as u32);
            for _ in 0..data_acked {
                s.send_buf.pop_front();
            }
            let extra = acked - data_acked;
            if extra > 0 && s.fin_sent {
                // FIN byte acked.
                s.fin_sent = false;
                s.fin_pending = false;
                match s.state {
                    State::FinWait1 => s.state = State::FinWait2,
                    State::LastAck => s.state = State::Closed,
                    _ => {}
                }
            }
            s.snd_una = ack;
            s.snd_wnd = window;
            s.retries = 0;
            s.last_send_us = now_us();
            closed_now = s.state == State::Closed;
        }
        if closed_now {
            free_socket(idx);
        }
    }
}

#[allow(static_mut_refs)]
fn process_segment_data(idx: usize, seq: u32, data: &[u8], flags: u8) {
    unsafe {
        let s = match SOCKETS.get_mut(idx).and_then(|s| s.as_mut()) {
            Some(s) => s,
            None => return,
        };
        if seq_eq(seq, s.rcv_nxt) {
            // In order: append data.
            for &b in data {
                s.recv_buf.push_back(b);
            }
            s.rx_bytes += data.len() as u64;
            s.rcv_nxt = s.rcv_nxt.wrapping_add(data.len() as u32);

            if flags & FLAG_FIN != 0 {
                s.rcv_nxt = s.rcv_nxt.wrapping_add(1);
                match s.state {
                    State::Established => s.state = State::CloseWait,
                    State::FinWait1 => s.state = State::Closed,   // collapsed from CLOSING + TIME_WAIT
                    State::FinWait2 => s.state = State::Closed,   // collapsed from TIME_WAIT
                    _ => {}
                }
            }

            // Drain reassembly buffer if the gap got filled.
            loop {
                let mut found_i: Option<usize> = None;
                for (i, (r_seq, _d)) in s.reassemble.iter().enumerate() {
                    if seq_eq(*r_seq, s.rcv_nxt) {
                        found_i = Some(i);
                        break;
                    }
                }
                if let Some(i) = found_i {
                    let (_, d) = s.reassemble.remove(i);
                    let d_len = d.len() as u32;
                    for b in d.iter() {
                        s.recv_buf.push_back(*b);
                    }
                    s.rx_bytes += d_len as u64;
                    s.rcv_nxt = s.rcv_nxt.wrapping_add(d_len);
                } else {
                    break;
                }
            }

            // ACK the new rcv_nxt right away.
            drop(s);
            emit_ack(idx);
        } else if seq_gt(seq, s.rcv_nxt) {
            // Future segment. Buffer if we have space.
            if !data.is_empty() && s.reassemble.len() < MAX_REASSEMBLE {
                // De-dup.
                let mut already = false;
                for (r_seq, _d) in s.reassemble.iter() {
                    if *r_seq == seq {
                        already = true;
                        break;
                    }
                }
                if !already {
                    s.reassemble.push((seq, data.to_vec()));
                }
            }
            // Duplicate-ACK the peer so they retransmit the gap.
            drop(s);
            emit_ack(idx);
        } else {
            // Old/duplicate — just re-ACK so the peer knows we're there.
            drop(s);
            emit_ack(idx);
        }
    }
}

#[allow(static_mut_refs)]
fn emit_ack(idx: usize) {
    unsafe {
        let s = match SOCKETS.get(idx).and_then(|s| s.as_ref()) {
            Some(s) => s,
            None => return,
        };
        // Pure ACK with no data and whatever SYN/FIN state we're not in-flight for.
        let flags = FLAG_ACK;
        let win = rcv_wnd_left(s);
        emit_segment(
            s.remote_ip,
            s.peer_mac,
            s.local_port,
            s.remote_port,
            s.snd_una.wrapping_add(current_snd_offset(s)),
            s.rcv_nxt,
            flags,
            win,
            &[],
        );
    }
}

/// Offset from snd_una to "next seq past what's in flight". Equal to:
///   (SYN-in-flight ? 1 : 0) + send_buf.len() + (FIN-in-flight ? 1 : 0)
fn current_snd_offset(s: &TcpSocket) -> u32 {
    let mut off = 0u32;
    if s.state == State::SynReceived || s.state == State::SynSent {
        off += 1;
    }
    off += s.send_buf.len() as u32;
    if s.fin_sent {
        off += 1;
    }
    off
}

fn rcv_wnd_left(_s: &TcpSocket) -> u16 {
    // Simple fixed-size window minus buffered data — we use a big buffer so
    // the window is effectively always open for small streams.
    let remaining = DEFAULT_RCV_WND as usize;
    remaining as u16
}

// ── Transmit: send the "current unacked state" as a segment ──────────────────

#[allow(static_mut_refs)]
fn transmit(idx: usize) -> bool {
    unsafe {
        let s = match SOCKETS.get_mut(idx).and_then(|s| s.as_mut()) {
            Some(s) => s,
            None => return false,
        };
        let mut flags = 0u8;
        let seq = s.snd_una;
        let mut data_len = 0usize;

        let mut include_syn = false;
        let mut include_fin = false;
        let mut include_ack = true;

        match s.state {
            State::SynSent => {
                include_syn = true;
                include_ack = false;
            }
            State::SynReceived => {
                include_syn = true;
            }
            State::Established | State::CloseWait => {
                let max = (MSS as usize).min(s.snd_wnd.max(1) as usize);
                data_len = s.send_buf.len().min(max);
            }
            State::FinWait1 | State::LastAck => {
                let max = (MSS as usize).min(s.snd_wnd.max(1) as usize);
                data_len = s.send_buf.len().min(max);
                if s.fin_pending && data_len == s.send_buf.len() {
                    include_fin = true;
                }
            }
            State::FinWait2 | State::Closed | State::Listen => {
                return false;
            }
        }

        // In ESTABLISHED / CLOSE_WAIT, piggy-back FIN if pending.
        if !include_fin
            && s.fin_pending
            && (s.state == State::Established || s.state == State::CloseWait)
            && data_len == s.send_buf.len()
        {
            // We need to actually transition here too. Only reached via close() from
            // these states — close() already transitions state.
        }

        if include_syn { flags |= FLAG_SYN; }
        if include_fin { flags |= FLAG_FIN; }
        if include_ack { flags |= FLAG_ACK; }

        // Collect data bytes (up to data_len from front of send_buf).
        let mut data_buf: Vec<u8> = Vec::with_capacity(data_len);
        for i in 0..data_len {
            data_buf.push(s.send_buf[i]);
        }

        let win = rcv_wnd_left(s);
        let remote_ip = s.remote_ip;
        let peer_mac = s.peer_mac;
        let local_port = s.local_port;
        let remote_port = s.remote_port;
        let rcv_nxt = s.rcv_nxt;

        // Track FIN state.
        if include_fin {
            s.fin_sent = true;
        }
        s.last_send_us = now_us();
        s.ack_pending = false;

        drop(s);
        emit_segment(
            remote_ip,
            peer_mac,
            local_port,
            remote_port,
            seq,
            rcv_nxt,
            flags,
            win,
            &data_buf,
        );
    }
    true
}

// ── Retransmit timer ─────────────────────────────────────────────────────────

#[allow(static_mut_refs)]
pub fn tick() {
    unsafe {
        let now = now_us();
        for i in 0..MAX_SOCKETS {
            let retrans;
            let timeout;
            {
                let s = match SOCKETS.get(i).and_then(|s| s.as_ref()) {
                    Some(s) => s,
                    None => continue,
                };
                // What's "in flight" that needs retransmit?
                let has_unacked = match s.state {
                    State::SynSent | State::SynReceived => true,
                    State::Established | State::CloseWait => s.send_buf.len() > 0,
                    State::FinWait1 | State::LastAck => s.send_buf.len() > 0 || s.fin_sent,
                    _ => false,
                };
                if !has_unacked {
                    continue;
                }
                if s.last_send_us == 0 {
                    continue;
                }
                timeout = now.saturating_sub(s.last_send_us) >= RETRANS_TIMEOUT_US;
                retrans = timeout;
            }
            if retrans {
                let abandon;
                {
                    let s = SOCKETS[i].as_mut().unwrap();
                    s.retries += 1;
                    abandon = s.retries > MAX_RETRIES;
                    TCP_STATS.retransmits += 1;
                    // When we retransmit FIN, we need to reset fin_sent so transmit() re-emits it.
                    s.fin_sent = false;
                }
                if abandon {
                    let s = SOCKETS[i].as_mut().unwrap();
                    send_rst(s.remote_ip, s.peer_mac, s.local_port, s.remote_port, s.snd_una, s.rcv_nxt);
                    TCP_STATS.resets_tx += 1;
                    free_socket(i);
                } else {
                    transmit(i);
                }
            }
        }
    }
}

// ── Wire format: emit + checksum ─────────────────────────────────────────────

fn emit_segment(
    dst_ip: [u8; 4],
    dst_mac: [u8; 6],
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    window: u16,
    data: &[u8],
) {
    let total_len = TCP_HDR_MIN + data.len();
    if total_len > 1500 - 20 {
        return;
    }
    let mut seg = [0u8; 1500];
    seg[0..2].copy_from_slice(&src_port.to_be_bytes());
    seg[2..4].copy_from_slice(&dst_port.to_be_bytes());
    seg[4..8].copy_from_slice(&seq.to_be_bytes());
    seg[8..12].copy_from_slice(&ack.to_be_bytes());
    // Data offset = 5 (5 × 4 bytes = 20 bytes).
    seg[12] = 5 << 4;
    seg[13] = flags;
    seg[14..16].copy_from_slice(&window.to_be_bytes());
    // Checksum placeholder
    seg[16] = 0;
    seg[17] = 0;
    // Urgent pointer = 0
    seg[18] = 0;
    seg[19] = 0;
    // Data
    if !data.is_empty() {
        seg[TCP_HDR_MIN..TCP_HDR_MIN + data.len()].copy_from_slice(data);
    }
    // Checksum over pseudo-header + segment.
    let csum = tcp_checksum(OUR_IP, dst_ip, &seg[..total_len]);
    seg[16..18].copy_from_slice(&csum.to_be_bytes());

    unsafe {
        TCP_STATS.tx_segments += 1;
        TCP_STATS.tx_bytes += data.len() as u64;
    }
    ip::send(dst_ip, dst_mac, ip::PROTO_TCP, &seg[..total_len]);
}

fn send_rst(
    dst_ip: [u8; 4],
    dst_mac: [u8; 6],
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
) {
    let flags = FLAG_RST | FLAG_ACK;
    emit_segment(dst_ip, dst_mac, src_port, dst_port, seq, ack, flags, 0, &[]);
}

fn verify_checksum(src_ip: [u8; 4], dst_ip: [u8; 4], segment: &[u8]) -> bool {
    // Sum the pseudo-header + segment (with the on-wire checksum field included).
    // A correct checksum will cause ~sum == 0.
    let mut sum: u32 = 0;
    sum += u16::from_be_bytes([src_ip[0], src_ip[1]]) as u32;
    sum += u16::from_be_bytes([src_ip[2], src_ip[3]]) as u32;
    sum += u16::from_be_bytes([dst_ip[0], dst_ip[1]]) as u32;
    sum += u16::from_be_bytes([dst_ip[2], dst_ip[3]]) as u32;
    sum += 0x0006u32; // zero + proto=6
    sum += segment.len() as u32;
    let mut i = 0;
    while i + 1 < segment.len() {
        sum += u16::from_be_bytes([segment[i], segment[i + 1]]) as u32;
        i += 2;
    }
    if i < segment.len() {
        sum += (segment[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16) == 0
}

fn tcp_checksum(src_ip: [u8; 4], dst_ip: [u8; 4], segment: &[u8]) -> u16 {
    // Segment already has checksum bytes = 0.
    let mut sum: u32 = 0;
    // Pseudo header: src ip (2 × u16), dst ip (2 × u16), zero+proto, length.
    sum += u16::from_be_bytes([src_ip[0], src_ip[1]]) as u32;
    sum += u16::from_be_bytes([src_ip[2], src_ip[3]]) as u32;
    sum += u16::from_be_bytes([dst_ip[0], dst_ip[1]]) as u32;
    sum += u16::from_be_bytes([dst_ip[2], dst_ip[3]]) as u32;
    sum += 0x0006u32;
    sum += segment.len() as u32;
    // Segment body.
    let mut i = 0;
    while i + 1 < segment.len() {
        sum += u16::from_be_bytes([segment[i], segment[i + 1]]) as u32;
        i += 2;
    }
    if i < segment.len() {
        sum += (segment[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

// ── Socket / listener table helpers ──────────────────────────────────────────

#[allow(static_mut_refs)]
fn find_socket(remote_ip: [u8; 4], remote_port: u16, local_port: u16) -> Option<usize> {
    unsafe {
        for (i, slot) in SOCKETS.iter().enumerate() {
            if let Some(s) = slot {
                if s.remote_ip == remote_ip && s.remote_port == remote_port && s.local_port == local_port {
                    return Some(i);
                }
            }
        }
        None
    }
}

#[allow(static_mut_refs)]
fn find_listener(port: u16) -> Option<usize> {
    unsafe {
        for (i, slot) in LISTENERS.iter().enumerate() {
            if let Some(l) = slot {
                if l.port == port {
                    return Some(i);
                }
            }
        }
        None
    }
}

#[allow(static_mut_refs)]
fn create_syn_received_socket(
    remote_ip: [u8; 4],
    remote_mac: [u8; 6],
    remote_port: u16,
    local_port: u16,
    peer_seq: u32,
    peer_window: u16,
) -> Option<usize> {
    unsafe {
        let idx = {
            let mut found = None;
            for (i, slot) in SOCKETS.iter().enumerate() {
                if slot.is_none() {
                    found = Some(i);
                    break;
                }
            }
            found?
        };
        let iss = (now_us() as u32).wrapping_mul(2654435761).wrapping_add(0xDEADu32);
        let sock = TcpSocket {
            local_port,
            remote_ip,
            remote_port,
            peer_mac: remote_mac,
            state: State::SynReceived,
            iss,
            snd_una: iss,
            snd_wnd: peer_window,
            irs: peer_seq,
            rcv_nxt: peer_seq.wrapping_add(1),
            send_buf: VecDeque::new(),
            recv_buf: VecDeque::new(),
            reassemble: Vec::new(),
            fin_pending: false,
            fin_sent: false,
            last_send_us: 0,
            retries: 0,
            ack_pending: false,
            accepted: false,
            handed_out: false,
            rx_bytes: 0,
            tx_bytes: 0,
            node_id: 0,
        };
        SOCKETS[idx] = Some(sock);
        Some(idx)
    }
}

#[allow(static_mut_refs)]
fn free_socket(idx: usize) {
    unsafe {
        // Remove from any listener's backlog.
        for slot in LISTENERS.iter_mut() {
            if let Some(l) = slot {
                l.backlog.retain(|&i| i != idx);
            }
        }
        // Unregister graph node.
        if let Some(s) = SOCKETS.get(idx).and_then(|s| s.as_ref()) {
            if s.node_id != 0 {
                crate::graph::get_mut().remove_node(s.node_id);
            }
        }
        SOCKETS[idx] = None;
        TCP_STATS.closes += 1;
    }
}

// ── Seq number comparison (modular) ──────────────────────────────────────────

fn seq_eq(a: u32, b: u32) -> bool { a == b }

fn seq_gt(a: u32, b: u32) -> bool {
    // a > b in 32-bit modular arithmetic.
    (a.wrapping_sub(b) as i32) > 0
}

// ── Graph integration ────────────────────────────────────────────────────────

fn register_listener_node(port: u16) -> u64 {
    let g = crate::graph::get_mut();
    let net0 = super::net0_node_id();
    if net0 == 0 {
        return 0;
    }
    let name = format!("tcp:listen:{}", port);
    let id = g.create_node(crate::graph::NodeType::System, &name);
    g.add_edge(net0, "child", id);
    if let Some(node) = g.get_node_mut(id) {
        node.content = format!(
            "TCP listener\nPort: {}\nState: LISTEN",
            port
        ).into_bytes();
    }
    id
}

#[allow(static_mut_refs)]
fn register_socket_node(idx: usize) {
    unsafe {
        let s = match SOCKETS.get_mut(idx).and_then(|s| s.as_mut()) {
            Some(s) => s,
            None => return,
        };
        if s.node_id != 0 {
            update_socket_node(idx);
            return;
        }
        let net0 = super::net0_node_id();
        if net0 == 0 {
            return;
        }
        let name = format!(
            "tcp:{}:{}.{}.{}.{}:{}",
            s.local_port,
            s.remote_ip[0], s.remote_ip[1], s.remote_ip[2], s.remote_ip[3],
            s.remote_port
        );
        let g = crate::graph::get_mut();
        let id = g.create_node(crate::graph::NodeType::System, &name);
        g.add_edge(net0, "child", id);
        s.node_id = id;
        drop(s);
        update_socket_node(idx);
    }
}

#[allow(static_mut_refs)]
pub fn update_socket_node(idx: usize) {
    unsafe {
        let s = match SOCKETS.get(idx).and_then(|s| s.as_ref()) {
            Some(s) => s,
            None => return,
        };
        if s.node_id == 0 { return; }
        let info = format!(
            "TCP connection\nLocal port: {}\nRemote: {}.{}.{}.{}:{}\nState: {}\nRX bytes: {}\nTX bytes: {}\nRecv buffered: {}\nSend pending: {}",
            s.local_port,
            s.remote_ip[0], s.remote_ip[1], s.remote_ip[2], s.remote_ip[3],
            s.remote_port,
            s.state.as_str(),
            s.rx_bytes, s.tx_bytes,
            s.recv_buf.len(), s.send_buf.len()
        );
        let g = crate::graph::get_mut();
        if let Some(node) = g.get_node_mut(s.node_id) {
            node.content = info.into_bytes();
        }
    }
}

// ── Introspection (for `tcp stats`) ──────────────────────────────────────────

#[allow(static_mut_refs)]
pub fn each_listener<F: FnMut(usize, &Listener)>(mut f: F) {
    unsafe {
        for (i, slot) in LISTENERS.iter().enumerate() {
            if let Some(l) = slot {
                f(i, l);
            }
        }
    }
}

#[allow(static_mut_refs)]
pub fn each_socket<F: FnMut(usize, &TcpSocket)>(mut f: F) {
    unsafe {
        for (i, slot) in SOCKETS.iter().enumerate() {
            if let Some(s) = slot {
                f(i, s);
            }
        }
    }
}

/// Stop listening on `port`. Returns true if the listener was removed.
#[allow(static_mut_refs)]
pub fn tcp_unlisten(port: u16) -> bool {
    unsafe {
        for slot in LISTENERS.iter_mut() {
            if let Some(l) = slot {
                if l.port == port {
                    if l.node_id != 0 {
                        crate::graph::get_mut().remove_node(l.node_id);
                    }
                    *slot = None;
                    return true;
                }
            }
        }
    }
    false
}

