/// Minimal HTTP/1.1 server — serves the Helios graph as JSON over TCP.
///
/// Design: the server is driven from the main polling loop via `http::tick()`.
/// When a listener is registered, each tick does a non-blocking `accept()` and
/// drains bytes from each accepted connection into a per-socket request buffer.
/// When the buffer contains `\r\n\r\n`, we parse the request line, route, build
/// a response, send it, and close the socket.
///
/// Close-after-response (HTTP/1.0 style) — no keep-alive, no chunked encoding.
/// Body is built in a single `String` up to a configurable cap.
///
/// Routes:
///   GET /ping         plain text "pong\n"
///   GET /             JSON overview
///   GET /stats        JSON {uptime, tick_count, heap, net, tcp, http}
///   GET /nodes        JSON array of {id, type, name, edges}
///   GET /nodes/{id}   JSON {id, type, name, content, edges:[...]}
///   GET /tree         JSON nested tree starting at root (bounded depth)

use super::json;
use super::tcp;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Max bytes we'll buffer per-request before giving up (and returning 400).
const MAX_REQ_BYTES: usize = 4096;
/// Max bytes we'll emit in a single response body.
const MAX_RESP_BYTES: usize = 64 * 1024;
/// Max concurrent in-flight HTTP connections we're willing to track.
const MAX_CONNS: usize = 16;
/// Bound the /tree traversal depth (guards against cycles + huge output).
const TREE_MAX_DEPTH: usize = 6;
/// Max nodes emitted in /tree response (hard cap, independent of depth).
const TREE_MAX_NODES: usize = 512;

/// A connection that has been accepted but hasn't finished receiving its
/// request yet (or has, and we're waiting to close).
struct Conn {
    sock: tcp::SocketHandle,
    /// Bytes read so far from the client.
    rxbuf: Vec<u8>,
    /// Start time (us) for request age / timeouts.
    start_us: u64,
}

pub struct HttpStats {
    pub requests: u64,
    pub bytes_out: u64,
    pub errors: u64,
    pub not_found: u64,
}

static mut STATS: HttpStats = HttpStats {
    requests: 0,
    bytes_out: 0,
    errors: 0,
    not_found: 0,
};

#[allow(static_mut_refs)]
pub fn stats() -> &'static HttpStats {
    unsafe { &STATS }
}

struct Server {
    port: u16,
    listener: tcp::ListenerHandle,
    conns: Vec<Conn>,
    /// Graph node ID for the server (http:server:<port>).
    node_id: u64,
}

static mut SERVER: Option<Server> = None;

#[allow(static_mut_refs)]
pub fn is_running() -> bool {
    unsafe { SERVER.is_some() }
}

#[allow(static_mut_refs)]
pub fn server_port() -> Option<u16> {
    unsafe { SERVER.as_ref().map(|s| s.port) }
}

/// Per-connection max-age (microseconds) before we force-close to avoid
/// leaking sockets if a client opens then never sends.
const CONN_MAX_AGE_US: u64 = 5_000_000;

/// Start the HTTP server on the given port.
/// Returns false if already running or the TCP listener couldn't be opened.
#[allow(static_mut_refs)]
pub fn start(port: u16) -> bool {
    if is_running() {
        return false;
    }
    let listener = match tcp::tcp_listen(port) {
        Some(h) => h,
        None => return false,
    };
    let node_id = register_server_node(port);
    let srv = Server {
        port,
        listener,
        conns: Vec::new(),
        node_id,
    };
    unsafe { SERVER = Some(srv); }
    update_server_node();
    true
}

/// Stop the HTTP server. Closes all in-flight connections and tears down
/// the TCP listener. Safe to call when not running (no-op).
#[allow(static_mut_refs)]
pub fn stop() {
    unsafe {
        let srv = match SERVER.take() {
            Some(s) => s,
            None => return,
        };
        // Close all in-flight conns.
        for c in srv.conns.iter() {
            tcp::close(c.sock);
        }
        // Remove listener.
        tcp::tcp_unlisten(srv.port);
        // Remove graph node.
        if srv.node_id != 0 {
            crate::graph::get_mut().remove_node(srv.node_id);
        }
    }
}

/// Called from the main loop each iteration. No-op if not running.
/// Accepts new connections, drains rx bytes, parses complete requests,
/// builds + sends responses, closes.
#[allow(static_mut_refs)]
pub fn tick() {
    unsafe {
        let srv = match SERVER.as_mut() {
            Some(s) => s,
            None => return,
        };

        // Accept new connections into our conn table.
        loop {
            if srv.conns.len() >= MAX_CONNS {
                break;
            }
            match tcp::accept(srv.listener) {
                Some(sock) => {
                    srv.conns.push(Conn {
                        sock,
                        rxbuf: Vec::new(),
                        start_us: tcp::now_us(),
                    });
                }
                None => break,
            }
        }

        // Drain / process each connection.
        let mut i = 0;
        while i < srv.conns.len() {
            let drop_it = process_conn(&mut srv.conns[i]);
            if drop_it {
                srv.conns.swap_remove(i);
            } else {
                i += 1;
            }
        }
    }
    // Refresh server graph node with new counts (cheap; happens each tick
    // while the server is running).
    update_server_node();
}

/// Returns true if the conn should be dropped from the tracking table.
/// Note: `send_full` / `send_simple` both call tcp::close() internally — after
/// calling them, we're done with this conn and return true. The TCP stack
/// drives the FIN handshake to completion on its own.
fn process_conn(c: &mut Conn) -> bool {
    // Check socket state — if the peer already RST'd, give up.
    let state = tcp::socket_state(c.sock);
    if state.is_none() {
        return true;
    }

    // Age timeout — avoid leaking sockets on half-open clients.
    let age = tcp::now_us().saturating_sub(c.start_us);
    if age > CONN_MAX_AGE_US {
        unsafe { STATS.errors += 1; }
        tcp::close(c.sock);
        return true;
    }

    // Drain rx bytes.
    let mut tmp = [0u8; 1024];
    let mut peer_closed = false;
    loop {
        match tcp::recv(c.sock, &mut tmp) {
            Some(0) => {
                // Peer closed (half-closed). We can still try to parse what
                // we have — curl sends request then closes its write side.
                peer_closed = true;
                break;
            }
            Some(n) => {
                if c.rxbuf.len() + n > MAX_REQ_BYTES {
                    send_simple(c.sock, 413, "Payload Too Large", "request too large\n");
                    unsafe { STATS.errors += 1; }
                    return true;
                }
                c.rxbuf.extend_from_slice(&tmp[..n]);
            }
            None => break, // no more data right now
        }
    }

    if !request_complete(&c.rxbuf) {
        if peer_closed {
            // Peer gave up without sending a full request.
            unsafe { STATS.errors += 1; }
            tcp::close(c.sock);
            return true;
        }
        // Keep waiting.
        return false;
    }

    // Parse + respond.
    match parse_request(&c.rxbuf) {
        Some((method, path)) => {
            unsafe { STATS.requests += 1; }
            let (status, status_text, content_type, body) = route(method, &path);
            if status == 404 {
                unsafe { STATS.not_found += 1; }
            }
            send_full(c.sock, status, status_text, content_type, &body);
        }
        None => {
            unsafe { STATS.errors += 1; }
            send_simple(c.sock, 400, "Bad Request", "malformed request\n");
        }
    }

    // Response sent and socket close() initiated inside send_full. Drop
    // our tracking entry — TCP will flush the FIN handshake asynchronously.
    true
}

/// Does the buffer contain a full request header block?
fn request_complete(buf: &[u8]) -> bool {
    // Look for CRLF CRLF.
    if buf.len() < 4 {
        return false;
    }
    for i in 0..buf.len() - 3 {
        if buf[i] == b'\r' && buf[i + 1] == b'\n' && buf[i + 2] == b'\r' && buf[i + 3] == b'\n' {
            return true;
        }
    }
    false
}

/// Parse just the request line. Returns (method, path).
fn parse_request(buf: &[u8]) -> Option<(String, String)> {
    // Find end of first line.
    let mut eol = None;
    for i in 0..buf.len().saturating_sub(1) {
        if buf[i] == b'\r' && buf[i + 1] == b'\n' {
            eol = Some(i);
            break;
        }
    }
    let eol = eol?;
    let line = core::str::from_utf8(&buf[..eol]).ok()?;
    let mut parts = line.splitn(3, ' ');
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let _version = parts.next()?;
    Some((method, path))
}

// ── Routing ──────────────────────────────────────────────────────────────────

/// Returns (status, status_text, content_type, body_bytes).
fn route(method: String, path: &str) -> (u16, &'static str, &'static str, Vec<u8>) {
    // Only GET/HEAD supported.
    if method != "GET" && method != "HEAD" {
        let body = b"405 method not allowed\n".to_vec();
        return (405, "Method Not Allowed", "text/plain; charset=utf-8", body);
    }

    // Strip query string (we don't use them yet).
    let path = match path.find('?') {
        Some(i) => &path[..i],
        None => path,
    };

    if path == "/ping" {
        return (200, "OK", "text/plain; charset=utf-8", b"pong\n".to_vec());
    }
    if path == "/" {
        return (200, "OK", "application/json; charset=utf-8", overview_json().into_bytes());
    }
    if path == "/stats" {
        return (200, "OK", "application/json; charset=utf-8", stats_json().into_bytes());
    }
    if path == "/nodes" {
        return (200, "OK", "application/json; charset=utf-8", nodes_json().into_bytes());
    }
    if path == "/tree" {
        return (200, "OK", "application/json; charset=utf-8", tree_json().into_bytes());
    }
    if let Some(rest) = path.strip_prefix("/nodes/") {
        if let Ok(id) = rest.parse::<u64>() {
            match node_json(id) {
                Some(body) => return (200, "OK", "application/json; charset=utf-8", body.into_bytes()),
                None => {
                    let body = alloc::format!("{{\"error\":\"node {} not found\"}}\n", id);
                    return (404, "Not Found", "application/json; charset=utf-8", body.into_bytes());
                }
            }
        }
    }

    let body = alloc::format!(
        "{{\"error\":\"not found\",\"path\":\"{}\"}}\n",
        escape_for_format(path)
    );
    (404, "Not Found", "application/json; charset=utf-8", body.into_bytes())
}

fn escape_for_format(s: &str) -> String {
    // Minimal escape so we can safely embed `path` in a JSON fragment built
    // with format!. Covers the few chars that would break the JSON.
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out
}

// ── JSON builders ────────────────────────────────────────────────────────────

fn overview_json() -> String {
    let g = crate::graph::get();
    let tick_count = crate::trap::tick_count() as u64;
    let mut out = String::with_capacity(512);
    let mut obj = json::ObjectBuilder::new(&mut out);
    obj.raw_field("helios", |o| {
        let mut h = json::ObjectBuilder::new(o);
        h.str_field("name", "Helios");
        h.str_field("version", env!("CARGO_PKG_VERSION"));
        h.str_field("motto", "Everything is a memory.");
        h.str_field("arch", "riscv64");
        h.finish();
    });
    obj.u64_field("node_count", g.node_count() as u64);
    obj.u64_field("edge_count", g.edge_count() as u64);
    obj.u64_field("tick_count", tick_count);
    obj.str_field(
        "net_ip",
        &alloc::format!(
            "{}.{}.{}.{}",
            super::OUR_IP[0],
            super::OUR_IP[1],
            super::OUR_IP[2],
            super::OUR_IP[3]
        ),
    );
    obj.raw_field("endpoints", |o| {
        let mut a = json::ArrayBuilder::new(o);
        a.str_item("/");
        a.str_item("/ping");
        a.str_item("/stats");
        a.str_item("/nodes");
        a.str_item("/nodes/{id}");
        a.str_item("/tree");
        a.finish();
    });
    obj.finish();
    out.push('\n');
    out
}

fn stats_json() -> String {
    let tick_count = crate::trap::tick_count() as u64;
    let time = crate::arch::riscv64::read_time() as u64;
    // QEMU virt timer is 10 MHz → microseconds = time / 10.
    let uptime_us = time / 10;
    let uptime_s = uptime_us / 1_000_000;

    let heap_used = crate::alloc_impl::heap_used() as u64;
    let heap_free = crate::alloc_impl::heap_free() as u64;
    let heap_total = crate::alloc_impl::heap_total() as u64;

    let net = super::stats();
    let tcp_s = tcp::stats();
    let http_s = unsafe { &STATS };

    let g = crate::graph::get();

    let mut out = String::with_capacity(1024);
    let mut obj = json::ObjectBuilder::new(&mut out);
    obj.u64_field("uptime_us", uptime_us);
    obj.u64_field("uptime_s", uptime_s);
    obj.u64_field("tick_count", tick_count);
    obj.raw_field("heap", |o| {
        let mut h = json::ObjectBuilder::new(o);
        h.u64_field("used", heap_used);
        h.u64_field("free", heap_free);
        h.u64_field("total", heap_total);
        h.finish();
    });
    obj.raw_field("graph", |o| {
        let mut h = json::ObjectBuilder::new(o);
        h.u64_field("nodes", g.node_count() as u64);
        h.u64_field("edges", g.edge_count() as u64);
        h.finish();
    });
    obj.raw_field("net", |o| {
        let mut h = json::ObjectBuilder::new(o);
        h.u64_field("rx_frames", net.rx_frames);
        h.u64_field("tx_frames", net.tx_frames);
        h.u64_field("arp_rx", net.arp_rx);
        h.u64_field("arp_tx", net.arp_tx);
        h.u64_field("icmp_rx", net.icmp_rx);
        h.u64_field("icmp_tx", net.icmp_tx);
        h.finish();
    });
    obj.raw_field("tcp", |o| {
        let mut h = json::ObjectBuilder::new(o);
        h.u64_field("rx_segments", tcp_s.rx_segments);
        h.u64_field("tx_segments", tcp_s.tx_segments);
        h.u64_field("rx_bytes", tcp_s.rx_bytes);
        h.u64_field("tx_bytes", tcp_s.tx_bytes);
        h.u64_field("retransmits", tcp_s.retransmits);
        h.u64_field("accepts", tcp_s.accepts);
        h.u64_field("closes", tcp_s.closes);
        h.u64_field("resets_rx", tcp_s.resets_rx);
        h.u64_field("resets_tx", tcp_s.resets_tx);
        h.finish();
    });
    obj.raw_field("http", |o| {
        let mut h = json::ObjectBuilder::new(o);
        h.u64_field("requests", http_s.requests);
        h.u64_field("bytes_out", http_s.bytes_out);
        h.u64_field("errors", http_s.errors);
        h.u64_field("not_found", http_s.not_found);
        h.finish();
    });
    obj.raw_field("tasks", |o| {
        let list = crate::task::list();
        let mut a = json::ArrayBuilder::new(o);
        for (id, name, state, preempts) in list.iter() {
            a.raw_item(|o2| {
                let mut t = json::ObjectBuilder::new(o2);
                t.u64_field("id", *id as u64);
                t.str_field("name", name);
                t.str_field("state", &state.to_string());
                t.u64_field("preempts", *preempts as u64);
                t.finish();
            });
        }
        a.finish();
    });
    obj.finish();
    out.push('\n');
    out
}

fn nodes_json() -> String {
    let g = crate::graph::get();
    let mut out = String::with_capacity(2048);
    // Top-level: { "count": N, "nodes": [ ... ] }
    let mut obj = json::ObjectBuilder::new(&mut out);
    obj.u64_field("count", g.node_count() as u64);
    obj.raw_field("nodes", |o| {
        let mut a = json::ArrayBuilder::new(o);
        for node in g.nodes.values() {
            a.raw_item(|o2| {
                let mut n = json::ObjectBuilder::new(o2);
                n.u64_field("id", node.id);
                n.str_field("type", &node.type_tag.to_string());
                n.str_field("name", &node.name);
                n.u64_field("edges", node.edges.len() as u64);
                n.u64_field("content_bytes", node.content.len() as u64);
                n.finish();
            });
        }
        a.finish();
    });
    obj.finish();
    out.push('\n');
    out
}

fn node_json(id: u64) -> Option<String> {
    let g = crate::graph::get();
    let node = g.get_node(id)?;
    let mut out = String::with_capacity(1024);
    let mut obj = json::ObjectBuilder::new(&mut out);
    obj.u64_field("id", node.id);
    obj.str_field("type", &node.type_tag.to_string());
    obj.str_field("name", &node.name);
    obj.u64_field("content_bytes", node.content.len() as u64);

    // Content — try UTF-8, fallback to byte count note.
    let content_str = node.display_content(g);
    // Truncate long content to keep responses reasonable.
    let truncated = if content_str.len() > 32 * 1024 {
        let mut t = String::new();
        t.push_str(&content_str[..32 * 1024]);
        t.push_str("\n... [truncated]");
        t
    } else {
        content_str
    };
    obj.str_field("content", &truncated);

    obj.raw_field("edges", |o| {
        let mut a = json::ArrayBuilder::new(o);
        for edge in node.edges.iter() {
            a.raw_item(|o2| {
                let mut e = json::ObjectBuilder::new(o2);
                e.str_field("label", &edge.label);
                e.u64_field("target", edge.target);
                // Include target name if it resolves.
                if let Some(tn) = g.get_node(edge.target) {
                    e.str_field("target_name", &tn.name);
                }
                e.finish();
            });
        }
        a.finish();
    });

    obj.finish();
    out.push('\n');
    Some(out)
}

fn tree_json() -> String {
    let mut out = String::with_capacity(4096);
    let mut visited: Vec<u64> = Vec::new();
    let mut emitted = 0usize;
    tree_node(&mut out, 1, 0, &mut visited, &mut emitted);
    out.push('\n');
    out
}

fn tree_node(out: &mut String, id: u64, depth: usize, visited: &mut Vec<u64>, emitted: &mut usize) {
    if out.len() > MAX_RESP_BYTES || *emitted >= TREE_MAX_NODES {
        json::null(out);
        return;
    }
    *emitted += 1;

    let g = crate::graph::get();
    let node = match g.get_node(id) {
        Some(n) => n,
        None => {
            json::null(out);
            return;
        }
    };

    if visited.contains(&id) {
        // Just a back-reference — emit a shallow stub.
        let mut obj = json::ObjectBuilder::new(out);
        obj.u64_field("id", node.id);
        obj.str_field("name", &node.name);
        obj.bool_field("cycle", true);
        obj.finish();
        return;
    }
    visited.push(id);

    {
        let mut obj = json::ObjectBuilder::new(out);
        obj.u64_field("id", node.id);
        obj.str_field("type", &node.type_tag.to_string());
        obj.str_field("name", &node.name);
        obj.u64_field("edges", node.edges.len() as u64);
        obj.raw_field("children", |o| {
            let mut a = json::ArrayBuilder::new(o);
            // Only follow "child" edges for the tree view, keep it tidy.
            let mut have_children = false;
            for edge in node.edges.iter() {
                if edge.label != "child" {
                    continue;
                }
                have_children = true;
                if depth + 1 >= TREE_MAX_DEPTH {
                    a.raw_item(|o2| {
                        let mut e = json::ObjectBuilder::new(o2);
                        e.u64_field("id", edge.target);
                        if let Some(tn) = g.get_node(edge.target) {
                            e.str_field("name", &tn.name);
                        }
                        e.bool_field("truncated", true);
                        e.finish();
                    });
                } else {
                    a.raw_item(|o2| {
                        tree_node(o2, edge.target, depth + 1, visited, emitted);
                    });
                }
            }
            let _ = have_children;
            a.finish();
        });
        obj.finish();
    }

    visited.pop();
}

// ── Wire I/O: response framing ───────────────────────────────────────────────

/// Send a simple plain-text response (e.g. error pages). Best-effort.
fn send_simple(sock: tcp::SocketHandle, status: u16, status_text: &str, body: &str) {
    send_full(
        sock,
        status,
        status_text,
        "text/plain; charset=utf-8",
        body.as_bytes(),
    );
}

fn send_full(sock: tcp::SocketHandle, status: u16, status_text: &str, content_type: &str, body: &[u8]) {
    // Cap body length.
    let body = if body.len() > MAX_RESP_BYTES {
        &body[..MAX_RESP_BYTES]
    } else {
        body
    };
    let header = alloc::format!(
        "HTTP/1.1 {} {}\r\n\
         Server: helios/{}\r\n\
         Content-Type: {}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n",
        status,
        status_text,
        env!("CARGO_PKG_VERSION"),
        content_type,
        body.len()
    );
    // Send header in small chunks (TCP send() takes one segment at a time up
    // to MSS; we call it in a loop so the send_buf absorbs everything).
    send_all(sock, header.as_bytes());
    send_all(sock, body);
    unsafe {
        STATS.bytes_out = STATS.bytes_out.saturating_add((header.len() + body.len()) as u64);
    }
    // Initiate close — the TCP stack will flush the FIN once the peer ACKs.
    tcp::close(sock);
}

/// TCP send() queues bytes into the send_buf; call it in a loop in case the
/// implementation caps at MSS.
fn send_all(sock: tcp::SocketHandle, mut data: &[u8]) {
    while !data.is_empty() {
        let n = tcp::send(sock, data);
        if n == 0 {
            // Socket state likely not allowing sends — bail.
            break;
        }
        data = &data[n..];
    }
}

// ── Graph node for the server ────────────────────────────────────────────────

fn register_server_node(port: u16) -> u64 {
    let net0 = super::net0_node_id();
    if net0 == 0 {
        return 0;
    }
    let name = alloc::format!("http:server:{}", port);
    let g = crate::graph::get_mut();
    let id = g.create_node(crate::graph::NodeType::System, &name);
    g.add_edge(net0, "child", id);
    id
}

#[allow(static_mut_refs)]
fn update_server_node() {
    unsafe {
        let srv = match SERVER.as_ref() {
            Some(s) => s,
            None => return,
        };
        if srv.node_id == 0 {
            return;
        }
        let s = &STATS;
        let info = alloc::format!(
            "HTTP server\n\
             Port: {}\n\
             Conns in flight: {}\n\
             Requests: {}\n\
             Bytes out: {}\n\
             404s: {}\n\
             Errors: {}",
            srv.port,
            srv.conns.len(),
            s.requests,
            s.bytes_out,
            s.not_found,
            s.errors
        );
        let g = crate::graph::get_mut();
        if let Some(node) = g.get_node_mut(srv.node_id) {
            node.content = info.into_bytes();
        }
    }
}
