/// Minimal HTTP/1.1 server — serves the Helios graph as JSON over TCP,
/// and (M27) accepts write operations to mutate it from outside.
///
/// Design: the server is driven from the main polling loop via `http::tick()`.
/// When a listener is registered, each tick does a non-blocking `accept()` and
/// drains bytes from each accepted connection into a per-socket request buffer.
/// We parse headers as soon as `\r\n\r\n` shows up; if Content-Length is set
/// we keep reading until the full body is in the buffer, then route.
///
/// Close-after-response (HTTP/1.0 style) — no keep-alive, no chunked encoding.
/// Response body is built in a single `Vec<u8>` up to a configurable cap.
///
/// Routes:
///   GET    /ping                    plain text "pong\n"
///   GET    /                        JSON overview
///   GET    /stats                   JSON {uptime, tick_count, heap, net, tcp, http}
///   GET    /nodes                   JSON array of {id, type, name, edges}
///   GET    /nodes/{id}              JSON {id, type, name, content, edges:[...]}
///   GET    /tree                    JSON nested tree starting at root
///   POST   /nodes                   create a node under /user
///                                     body (form): type=note&name=X&content=Y
///                                     → 201 {id, name, type}
///   PUT    /nodes/{id}/content      replace node content (body = raw bytes)
///                                     → 200 {id, content_bytes}
///   DELETE /nodes/{id}              remove node + edges → 200 {deleted}
///   POST   /nodes/{id}/edges        add edge, body (form): target=N[&label=L]
///                                     → 200 {from, to, label}
///
/// Write protections:
///   * Nodes with ID ≤ 15 are system-managed and refuse writes/deletes (403).
///   * PUT /content only succeeds on text/dir types, or nodes that we tracked
///     as externally-created (see graph::user).

use super::json;
use super::tcp;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Max bytes we'll buffer per-request (headers + body combined).
const MAX_REQ_BYTES: usize = 64 * 1024;
/// Max bytes we'll accept as a request body after Content-Length parsing.
const MAX_BODY_BYTES: usize = 32 * 1024;
/// Max bytes we'll emit in a single response body.
const MAX_RESP_BYTES: usize = 64 * 1024;
/// Max concurrent in-flight HTTP connections we're willing to track.
const MAX_CONNS: usize = 16;
/// Bound the /tree traversal depth (guards against cycles + huge output).
const TREE_MAX_DEPTH: usize = 6;
/// Max nodes emitted in /tree response (hard cap, independent of depth).
const TREE_MAX_NODES: usize = 512;
/// IDs at or below this are considered system-managed and protected from
/// external writes/deletes. Bootstrap creates IDs 1..=11 or so (root, system,
/// devices, uart0, fb0, memory, timer, cpu, dashboard, net0, ipc, user).
/// Keep a little slack for the http server node itself.
const PROTECTED_MAX_ID: u64 = 15;

/// A connection that has been accepted but hasn't finished receiving its
/// request yet (or has, and we're waiting to close).
struct Conn {
    sock: tcp::SocketHandle,
    /// Bytes read so far from the client.
    rxbuf: Vec<u8>,
    /// Start time (us) for request age / timeouts.
    start_us: u64,
    /// After the blank line is seen: (body_start_offset, content_length).
    /// body_start is the index just past the `\r\n\r\n` separator.
    parsed: Option<(usize, usize)>,
}

pub struct HttpStats {
    pub requests: u64,
    pub bytes_out: u64,
    pub errors: u64,
    pub not_found: u64,
    pub writes: u64,
}

static mut STATS: HttpStats = HttpStats {
    requests: 0,
    bytes_out: 0,
    errors: 0,
    not_found: 0,
    writes: 0,
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
        for c in srv.conns.iter() {
            tcp::close(c.sock);
        }
        tcp::tcp_unlisten(srv.port);
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
                        parsed: None,
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
fn process_conn(c: &mut Conn) -> bool {
    // Check socket state — if the peer already RST'd, give up.
    if tcp::socket_state(c.sock).is_none() {
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
                // Peer closed (half-closed). Try to parse what we have.
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
            None => break,
        }
    }

    // Have we seen the end of headers yet?
    if c.parsed.is_none() {
        match find_header_end(&c.rxbuf) {
            Some(hend) => {
                let cl = parse_content_length(&c.rxbuf[..hend]).unwrap_or(0);
                if cl > MAX_BODY_BYTES {
                    send_simple(c.sock, 413, "Payload Too Large", "body too large\n");
                    unsafe { STATS.errors += 1; }
                    return true;
                }
                c.parsed = Some((hend, cl));
            }
            None => {
                if peer_closed {
                    unsafe { STATS.errors += 1; }
                    tcp::close(c.sock);
                    return true;
                }
                return false; // keep waiting
            }
        }
    }

    let (hend, cl) = c.parsed.unwrap();

    // Do we have the full body yet?
    if c.rxbuf.len() < hend + cl {
        if peer_closed {
            // Client closed without finishing the body → abort.
            unsafe { STATS.errors += 1; }
            tcp::close(c.sock);
            return true;
        }
        return false;
    }

    // Full request is buffered. Parse request line, route, respond.
    let (method, path) = match parse_request_line(&c.rxbuf[..hend]) {
        Some(v) => v,
        None => {
            unsafe { STATS.errors += 1; }
            send_simple(c.sock, 400, "Bad Request", "malformed request\n");
            return true;
        }
    };
    let body = &c.rxbuf[hend..hend + cl];
    let peer_ip = tcp::socket_peer(c.sock).map(|(ip, _)| ip).unwrap_or([0; 4]);

    unsafe { STATS.requests += 1; }
    let (status, status_text, content_type, response_body) =
        route(&method, &path, body, peer_ip);
    match status {
        404 => unsafe { STATS.not_found += 1; },
        200 | 201 if method != "GET" && method != "HEAD" => unsafe { STATS.writes += 1; },
        _ => {}
    }
    send_full(c.sock, status, status_text, content_type, &response_body);
    true
}

// ── Header / body parsing ────────────────────────────────────────────────────

/// Locate the end of the header block (the first byte of the body, just past
/// `\r\n\r\n`). Returns None if the blank line hasn't arrived yet.
fn find_header_end(buf: &[u8]) -> Option<usize> {
    if buf.len() < 4 {
        return None;
    }
    for i in 0..=buf.len() - 4 {
        if buf[i] == b'\r' && buf[i + 1] == b'\n'
            && buf[i + 2] == b'\r' && buf[i + 3] == b'\n'
        {
            return Some(i + 4);
        }
    }
    None
}

/// Find `Content-Length:` (case-insensitive) in the header block and return
/// its integer value. Missing/invalid → None.
fn parse_content_length(headers: &[u8]) -> Option<usize> {
    let s = core::str::from_utf8(headers).ok()?;
    for line in s.split("\r\n") {
        if let Some(pos) = line.find(':') {
            let key = &line[..pos];
            if key.eq_ignore_ascii_case("content-length") {
                return line[pos + 1..].trim().parse::<usize>().ok();
            }
        }
    }
    None
}

/// Parse just the request line. Returns (method, path) as owned Strings.
fn parse_request_line(buf: &[u8]) -> Option<(String, String)> {
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

// ── URL decoding + form-body parsing ─────────────────────────────────────────

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Decode a percent-encoded string (application/x-www-form-urlencoded flavor:
/// `+` → space, `%XX` → byte). Invalid escapes are passed through unchanged.
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'+' {
            out.push(b' ');
            i += 1;
        } else if b == b'%' && i + 2 < bytes.len() {
            match (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                (Some(hi), Some(lo)) => {
                    out.push((hi << 4) | lo);
                    i += 3;
                }
                _ => {
                    out.push(b);
                    i += 1;
                }
            }
        } else {
            out.push(b);
            i += 1;
        }
    }
    match String::from_utf8(out) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(&e.into_bytes()).into_owned(),
    }
}

/// Parse a form-encoded body into a `key → value` map. Keys/values are
/// URL-decoded; duplicate keys keep the last occurrence.
fn parse_form(body: &[u8]) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let s = match core::str::from_utf8(body) {
        Ok(s) => s,
        Err(_) => return map,
    };
    for pair in s.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.find('=') {
            Some(pos) => (&pair[..pos], &pair[pos + 1..]),
            None => (pair, ""),
        };
        map.insert(url_decode(k), url_decode(v));
    }
    map
}

// ── Routing ──────────────────────────────────────────────────────────────────

type RouteResult = (u16, &'static str, &'static str, Vec<u8>);

fn route(method: &str, path: &str, body: &[u8], peer_ip: [u8; 4]) -> RouteResult {
    // Strip query string (we don't use them yet).
    let path = match path.find('?') {
        Some(i) => &path[..i],
        None => path,
    };

    match method {
        "GET" | "HEAD" => route_read(path),
        "POST" => route_post(path, body, peer_ip),
        "PUT" => route_put(path, body),
        "DELETE" => route_delete(path),
        _ => err_plain(405, "Method Not Allowed", "405 method not allowed\n"),
    }
}

fn route_read(path: &str) -> RouteResult {
    if path == "/ping" {
        return ok_text("pong\n");
    }
    if path == "/" {
        return ok_json(overview_json().into_bytes());
    }
    if path == "/dashboard" || path == "/dashboard/" {
        return ok_html(super::dashboard::DASHBOARD_HTML.as_bytes().to_vec());
    }
    if path == "/stats" {
        return ok_json(stats_json().into_bytes());
    }
    if path == "/nodes" {
        return ok_json(nodes_json().into_bytes());
    }
    if path == "/tree" {
        return ok_json(tree_json().into_bytes());
    }
    if let Some(rest) = path.strip_prefix("/nodes/") {
        if let Ok(id) = rest.parse::<u64>() {
            match node_json(id) {
                Some(body) => return ok_json(body.into_bytes()),
                None => return not_found_id(id),
            }
        }
    }
    not_found_path(path)
}

fn route_post(path: &str, body: &[u8], peer_ip: [u8; 4]) -> RouteResult {
    if path == "/nodes" {
        return handle_create_node(body, peer_ip);
    }
    if let Some(rest) = path.strip_prefix("/nodes/") {
        if let Some(slash) = rest.find('/') {
            let id_str = &rest[..slash];
            let sub = &rest[slash..];
            if sub == "/edges" {
                if let Ok(id) = id_str.parse::<u64>() {
                    return handle_add_edge(id, body);
                }
            }
        }
    }
    not_found_path(path)
}

fn route_put(path: &str, body: &[u8]) -> RouteResult {
    if let Some(rest) = path.strip_prefix("/nodes/") {
        if let Some(slash) = rest.find('/') {
            let id_str = &rest[..slash];
            let sub = &rest[slash..];
            if sub == "/content" {
                if let Ok(id) = id_str.parse::<u64>() {
                    return handle_put_content(id, body);
                }
            }
        }
    }
    not_found_path(path)
}

fn route_delete(path: &str) -> RouteResult {
    if let Some(rest) = path.strip_prefix("/nodes/") {
        if let Ok(id) = rest.parse::<u64>() {
            return handle_delete_node(id);
        }
    }
    not_found_path(path)
}

// ── Write handlers ───────────────────────────────────────────────────────────

fn handle_create_node(body: &[u8], peer_ip: [u8; 4]) -> RouteResult {
    let form = parse_form(body);
    let type_str = form
        .get("type")
        .map(|s| s.as_str())
        .unwrap_or("note")
        .to_string();
    let name = match form.get("name") {
        Some(n) if !n.is_empty() => n.clone(),
        _ => return err_plain(400, "Bad Request", "missing 'name' field\n"),
    };
    let content = form.get("content").cloned().unwrap_or_default();

    let type_tag = match type_str.as_str() {
        "note" | "text" => crate::graph::NodeType::Text,
        "dir" | "directory" => crate::graph::NodeType::Directory,
        _ => {
            return err_plain(
                400,
                "Bad Request",
                "unsupported 'type'; use 'note' or 'dir'\n",
            )
        }
    };

    let user_dir = crate::graph::user::user_dir_id();
    if user_dir == 0 {
        return err_plain(
            500,
            "Internal Server Error",
            "/user subgraph not initialized\n",
        );
    }

    let g = crate::graph::get_mut();
    let id = g.create_node(type_tag, &name);
    if let Some(node) = g.get_node_mut(id) {
        node.content = content.into_bytes();
    }
    g.add_edge(user_dir, "child", id);
    crate::graph::user::register(id, peer_ip);

    let mut out = String::with_capacity(96);
    let mut obj = json::ObjectBuilder::new(&mut out);
    obj.u64_field("id", id);
    obj.str_field("name", &name);
    obj.str_field("type", &type_str);
    obj.finish();
    out.push('\n');
    (
        201,
        "Created",
        "application/json; charset=utf-8",
        out.into_bytes(),
    )
}

fn handle_put_content(id: u64, body: &[u8]) -> RouteResult {
    if id <= PROTECTED_MAX_ID {
        return err_plain(
            403,
            "Forbidden",
            "node is system-managed; writes refused\n",
        );
    }
    // Decide up-front whether this node is writable.
    let is_user = crate::graph::user::is_user_node(id);
    let g = crate::graph::get_mut();
    let node = match g.get_node_mut(id) {
        Some(n) => n,
        None => return not_found_id(id),
    };
    let type_ok = matches!(
        node.type_tag,
        crate::graph::NodeType::Text | crate::graph::NodeType::Directory
    );
    if !type_ok && !is_user {
        return err_plain(
            403,
            "Forbidden",
            "writes only allowed on note/dir or externally-created nodes\n",
        );
    }
    node.content = body.to_vec();
    let bytes = body.len() as u64;

    let mut out = String::with_capacity(64);
    let mut obj = json::ObjectBuilder::new(&mut out);
    obj.u64_field("id", id);
    obj.u64_field("content_bytes", bytes);
    obj.finish();
    out.push('\n');
    (
        200,
        "OK",
        "application/json; charset=utf-8",
        out.into_bytes(),
    )
}

fn handle_delete_node(id: u64) -> RouteResult {
    if id <= PROTECTED_MAX_ID {
        return err_plain(
            403,
            "Forbidden",
            "node is system-managed; deletion refused\n",
        );
    }
    let g = crate::graph::get_mut();
    if !g.nodes.contains_key(&id) {
        return not_found_id(id);
    }
    g.remove_node(id);
    crate::graph::user::forget(id);

    let mut out = String::with_capacity(32);
    let mut obj = json::ObjectBuilder::new(&mut out);
    obj.u64_field("deleted", id);
    obj.finish();
    out.push('\n');
    (
        200,
        "OK",
        "application/json; charset=utf-8",
        out.into_bytes(),
    )
}

fn handle_add_edge(parent_id: u64, body: &[u8]) -> RouteResult {
    let form = parse_form(body);
    let target = match form.get("target").and_then(|s| s.parse::<u64>().ok()) {
        Some(v) => v,
        None => return err_plain(400, "Bad Request", "missing or invalid 'target'\n"),
    };
    let label = form
        .get("label")
        .map(|s| s.as_str())
        .unwrap_or("child")
        .to_string();

    let g = crate::graph::get_mut();
    if !g.nodes.contains_key(&parent_id) {
        return not_found_id(parent_id);
    }
    if !g.nodes.contains_key(&target) {
        return not_found_id(target);
    }
    g.add_edge(parent_id, &label, target);

    let mut out = String::with_capacity(64);
    let mut obj = json::ObjectBuilder::new(&mut out);
    obj.u64_field("from", parent_id);
    obj.u64_field("to", target);
    obj.str_field("label", &label);
    obj.finish();
    out.push('\n');
    (
        200,
        "OK",
        "application/json; charset=utf-8",
        out.into_bytes(),
    )
}

// ── Small response-builder helpers ───────────────────────────────────────────

fn ok_text(s: &str) -> RouteResult {
    (200, "OK", "text/plain; charset=utf-8", s.as_bytes().to_vec())
}

fn ok_json(body: Vec<u8>) -> RouteResult {
    (200, "OK", "application/json; charset=utf-8", body)
}

fn ok_html(body: Vec<u8>) -> RouteResult {
    (200, "OK", "text/html; charset=utf-8", body)
}

fn err_plain(code: u16, text: &'static str, msg: &str) -> RouteResult {
    (
        code,
        text,
        "text/plain; charset=utf-8",
        msg.as_bytes().to_vec(),
    )
}

fn not_found_id(id: u64) -> RouteResult {
    let body = alloc::format!("{{\"error\":\"node {} not found\"}}\n", id);
    (
        404,
        "Not Found",
        "application/json; charset=utf-8",
        body.into_bytes(),
    )
}

fn not_found_path(path: &str) -> RouteResult {
    let body = alloc::format!(
        "{{\"error\":\"not found\",\"path\":\"{}\"}}\n",
        escape_for_format(path)
    );
    (
        404,
        "Not Found",
        "application/json; charset=utf-8",
        body.into_bytes(),
    )
}

fn escape_for_format(s: &str) -> String {
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

// ── JSON builders (reads) ────────────────────────────────────────────────────

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
    obj.str_field("dashboard", "/dashboard");
    obj.raw_field("endpoints", |o| {
        let mut a = json::ArrayBuilder::new(o);
        a.str_item("GET /");
        a.str_item("GET /ping");
        a.str_item("GET /dashboard");
        a.str_item("GET /stats");
        a.str_item("GET /nodes");
        a.str_item("GET /nodes/{id}");
        a.str_item("GET /tree");
        a.str_item("POST /nodes");
        a.str_item("PUT /nodes/{id}/content");
        a.str_item("DELETE /nodes/{id}");
        a.str_item("POST /nodes/{parent_id}/edges");
        a.finish();
    });
    obj.finish();
    out.push('\n');
    out
}

fn stats_json() -> String {
    let tick_count = crate::trap::tick_count() as u64;
    let time = crate::arch::riscv64::read_time() as u64;
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
        h.u64_field("user_nodes", crate::graph::user::count() as u64);
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
        h.u64_field("writes", http_s.writes);
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
                n.bool_field("user", crate::graph::user::is_user_node(node.id));
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
    obj.bool_field("user", crate::graph::user::is_user_node(node.id));

    let content_str = node.display_content(g);
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
            for edge in node.edges.iter() {
                if edge.label != "child" {
                    continue;
                }
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
            a.finish();
        });
        obj.finish();
    }

    visited.pop();
}

// ── Wire I/O: response framing ───────────────────────────────────────────────

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
         Cache-Control: no-cache\r\n\
         Connection: close\r\n\
         \r\n",
        status,
        status_text,
        env!("CARGO_PKG_VERSION"),
        content_type,
        body.len()
    );
    send_all(sock, header.as_bytes());
    send_all(sock, body);
    unsafe {
        STATS.bytes_out =
            STATS.bytes_out.saturating_add((header.len() + body.len()) as u64);
    }
    tcp::close(sock);
}

fn send_all(sock: tcp::SocketHandle, mut data: &[u8]) {
    while !data.is_empty() {
        let n = tcp::send(sock, data);
        if n == 0 {
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
             Writes: {}\n\
             Bytes out: {}\n\
             404s: {}\n\
             Errors: {}",
            srv.port,
            srv.conns.len(),
            s.requests,
            s.writes,
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
