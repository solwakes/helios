/// Interactive UART shell for Helios.

use crate::arch::riscv64 as arch;
use crate::trap;
use crate::graph::navigator::{self, NavInput};

const MAX_LINE: usize = 256;
const PROMPT: &str = "helios> ";
const EDIT_PROMPT: &str = "| ";

/// Timer frequency on QEMU virt (10 MHz).
const TIMER_FREQ: usize = 10_000_000;

static mut LINE_BUF: [u8; MAX_LINE] = [0u8; MAX_LINE];
static mut LINE_LEN: usize = 0;

/// Whether a script is currently executing (prevents recursive `run`).
static mut RUNNING_SCRIPT: bool = false;

/// Whether we are in edit mode.
static mut EDIT_MODE: bool = false;
/// The node ID being edited.
static mut EDIT_NODE_ID: u64 = 0;
/// Accumulation buffer for edit mode.
static mut EDIT_BUFFER: Option<alloc::vec::Vec<u8>> = None;

// ---------------------------------------------------------------------------
// Mode & escape sequence state
// ---------------------------------------------------------------------------

/// Whether we are in navigator mode (true) or shell mode (false).
static mut NAV_MODE: bool = false;

/// Escape sequence parser state.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EscState {
    Normal,
    GotEsc,   // received 0x1b
    GotBracket, // received 0x1b [
}

static mut ESC_STATE: EscState = EscState::Normal;

/// Print the prompt and mark the shell as active.
/// If a framebuffer is available, start in navigator mode.
pub fn init() {
    trap::set_shell_active();
    if crate::framebuffer::get().is_some() {
        enter_nav_mode();
    } else {
        crate::print!("{}", PROMPT);
    }
}

/// Enter navigator mode.
fn enter_nav_mode() {
    crate::console::set_active(false);
    navigator::init();
    navigator::get_mut().ensure_valid_selection();
    unsafe { NAV_MODE = true; }
    crate::println!("Entering graph navigator. Press 'q' or Escape to exit.");
    // Refresh live data and render
    crate::graph::live::refresh_system_nodes();
    navigator::render_nav();
}

/// Exit navigator mode — drop to the text console.
fn exit_nav_mode() {
    unsafe { NAV_MODE = false; }
    crate::console::set_active(true);
    crate::println!("Exited graph navigator.");
    crate::print!("{}", PROMPT);
}

/// Process a single byte received from the UART.
pub fn process_byte(byte: u8) {
    unsafe {
        // Escape sequence state machine (works in both modes)
        match ESC_STATE {
            EscState::GotEsc => {
                if byte == b'[' {
                    ESC_STATE = EscState::GotBracket;
                    return;
                }
                // Not a CSI sequence — handle ESC alone
                ESC_STATE = EscState::Normal;
                if NAV_MODE {
                    // Bare Escape => quit
                    exit_nav_mode();
                    return;
                }
                // In shell mode, ignore bare escape
                return;
            }
            EscState::GotBracket => {
                ESC_STATE = EscState::Normal;
                if NAV_MODE {
                    let input = match byte {
                        b'A' => Some(NavInput::Up),
                        b'B' => Some(NavInput::Down),
                        b'C' => Some(NavInput::Right),
                        b'D' => Some(NavInput::Left),
                        _ => None,
                    };
                    if let Some(inp) = input {
                        handle_nav_input(inp);
                    }
                    return;
                }
                // In shell mode, ignore arrow keys
                return;
            }
            EscState::Normal => {
                if byte == 0x1b {
                    ESC_STATE = EscState::GotEsc;
                    return;
                }
            }
        }

        // Route to navigator, edit mode, or shell
        if NAV_MODE {
            process_nav_byte(byte);
        } else if EDIT_MODE {
            process_edit_byte(byte);
        } else {
            process_shell_byte(byte);
        }
    }
}

/// Process a byte in navigator mode.
fn process_nav_byte(byte: u8) {
    // 't' switches directly to the framebuffer text console
    if byte == b't' {
        unsafe { NAV_MODE = false; }
        crate::console::set_active(true);
        crate::println!("Framebuffer console active. Type 'render' for graph view.");
        crate::print!("{}", PROMPT);
        return;
    }
    let input = match byte {
        b'q' => Some(NavInput::Quit),
        b'\r' | b'\n' | b' ' => Some(NavInput::ToggleCollapse),
        b'\t' | b'd' => Some(NavInput::ToggleDetail),
        b'r' => Some(NavInput::Refresh),
        _ => None,
    };
    if let Some(inp) = input {
        handle_nav_input(inp);
    }
}

/// Handle a navigator input action.
fn handle_nav_input(input: NavInput) {
    let nav = navigator::get_mut();
    match nav.handle_input(input) {
        None => {
            // Quit
            exit_nav_mode();
        }
        Some(true) => {
            // Re-render needed
            crate::graph::live::refresh_system_nodes();
            navigator::render_nav();
        }
        Some(false) => {
            // No change, skip re-render
        }
    }
}

/// Process tablet (mouse) events — cursor movement and clicks.
/// Called from the main loop after polling the tablet device.
pub fn process_tablet_events() {
    use crate::virtio::tablet;

    let cur = tablet::cursor();
    let moved = cur.moved;
    let clicked = cur.left_clicked;
    let pressed = cur.left_pressed;

    if !moved && !clicked {
        // If a drag was in flight and the button was released, end it.
        let wm = crate::graph::window::get_mut();
        if wm.is_dragging() && !pressed {
            wm.end_drag();
        }
        return;
    }

    let cx = cur.x;
    let cy = cur.y;

    // Clear the event flags
    if moved { tablet::clear_moved(); }
    if clicked { tablet::clear_click(); }

    unsafe {
        if NAV_MODE {
            // --- Window manager first ----------------------------------
            let wm = crate::graph::window::get_mut();

            // If a drag is in progress, follow the cursor, or end drag on release.
            if wm.is_dragging() {
                if !pressed {
                    wm.end_drag();
                    // Repaint to clean up any drag artifacts.
                    crate::graph::live::refresh_system_nodes();
                    navigator::render_nav();
                    return;
                }
                if moved {
                    let changed = wm.update_drag(cx as i32, cy as i32);
                    if changed {
                        // Repaint (windows only live on the navigator view).
                        crate::graph::live::refresh_system_nodes();
                        navigator::render_nav();
                    }
                }
                return;
            }

            // Clicks: window close / title drag / body focus
            if clicked {
                if let Some(close_id) = wm.hit_close(cx as i32, cy as i32) {
                    wm.close(close_id);
                    crate::graph::live::refresh_system_nodes();
                    navigator::render_nav();
                    return;
                }
                if let Some((hit_id, on_title)) = wm.hit_test(cx as i32, cy as i32) {
                    wm.focus(hit_id);
                    if on_title {
                        wm.begin_drag(hit_id, cx as i32, cy as i32);
                    }
                    crate::graph::live::refresh_system_nodes();
                    navigator::render_nav();
                    return;
                }
                // else: fall through to graph navigator click
            }

            if moved {
                // A moved-only event over a window: just redraw the cursor,
                // don't repaint the whole screen. But if the hover is on a
                // graph node (i.e. not on a window), do the existing hover.
                if wm.hit_test(cx as i32, cy as i32).is_some() {
                    if let Some(fb) = crate::framebuffer::get() {
                        crate::framebuffer::undraw_cursor(fb);
                        crate::framebuffer::draw_cursor(fb, cx, cy);
                    }
                    return;
                }
                // Otherwise fall through to navigator hover logic.
            }

            // --- Graph navigator behavior (no window was clicked/hovered) --
            let mut need_render = false;

            // Hit test against last layout
            if let Some(hit_id) = crate::graph::render::hit_test(cx, cy) {
                let nav = navigator::get_mut();

                if clicked {
                    // Click on a node: select it and toggle collapse
                    nav.selected_node = hit_id;
                    // Toggle collapse (same as Enter)
                    match nav.handle_input(NavInput::ToggleCollapse) {
                        None => {
                            exit_nav_mode();
                            return;
                        }
                        _ => {}
                    }
                    need_render = true;
                } else if moved && nav.selected_node != hit_id {
                    // Hover: highlight the node under cursor
                    nav.selected_node = hit_id;
                    need_render = true;
                }
            }

            if need_render {
                crate::graph::live::refresh_system_nodes();
                navigator::render_nav();
            } else if moved {
                // Just redraw cursor at new position
                if let Some(fb) = crate::framebuffer::get() {
                    crate::framebuffer::undraw_cursor(fb);
                    crate::framebuffer::draw_cursor(fb, cx, cy);
                }
            }
        }
    }
}

/// Process a byte in edit mode — accumulate lines into EDIT_BUFFER.
fn process_edit_byte(byte: u8) {
    unsafe {
        match byte {
            // Enter (CR or LF)
            0x0D | 0x0A => {
                crate::println!();
                let line = core::str::from_utf8_unchecked(&LINE_BUF[..LINE_LEN]);
                if line.is_empty() {
                    // Empty line => finish editing
                    let buf = EDIT_BUFFER.take().unwrap_or_default();
                    let byte_count = buf.len();
                    let node_id = EDIT_NODE_ID;
                    EDIT_MODE = false;

                    let g = crate::graph::get_mut();
                    match g.get_node_mut(node_id) {
                        Some(node) => {
                            node.content = buf;
                            crate::println!("Node #{} updated ({} bytes)", node_id, byte_count);
                        }
                        None => {
                            crate::println!("Node #{} not found (edit discarded)", node_id);
                        }
                    }
                    LINE_LEN = 0;
                    crate::print!("{}", PROMPT);
                } else {
                    // Append this line to the edit buffer
                    if let Some(ref mut buf) = EDIT_BUFFER {
                        if !buf.is_empty() {
                            buf.push(b'\n');
                        }
                        buf.extend_from_slice(&LINE_BUF[..LINE_LEN]);
                    }
                    LINE_LEN = 0;
                    crate::print!("{}", EDIT_PROMPT);
                }
            }
            // Backspace (DEL or BS)
            0x7F | 0x08 => {
                if LINE_LEN > 0 {
                    LINE_LEN -= 1;
                    crate::uart::putc(0x08);
                    crate::uart::putc(b' ');
                    crate::uart::putc(0x08);
                    crate::console::putc(0x08);
                    crate::console::putc(b' ');
                    crate::console::putc(0x08);
                }
            }
            // Ctrl-D => finish editing (like empty line)
            0x04 => {
                crate::println!();
                let buf = EDIT_BUFFER.take().unwrap_or_default();
                let byte_count = buf.len();
                let node_id = EDIT_NODE_ID;
                EDIT_MODE = false;

                let g = crate::graph::get_mut();
                match g.get_node_mut(node_id) {
                    Some(node) => {
                        node.content = buf;
                        crate::println!("Node #{} updated ({} bytes)", node_id, byte_count);
                    }
                    None => {
                        crate::println!("Node #{} not found (edit discarded)", node_id);
                    }
                }
                LINE_LEN = 0;
                crate::print!("{}", PROMPT);
            }
            // Printable ASCII
            0x20..=0x7E => {
                if LINE_LEN < MAX_LINE {
                    LINE_BUF[LINE_LEN] = byte;
                    LINE_LEN += 1;
                    crate::uart::putc(byte);
                    crate::console::putc(byte);
                }
            }
            _ => {}
        }
    }
}

/// Process a byte in shell mode.
fn process_shell_byte(byte: u8) {
    unsafe {
        match byte {
            // Enter (CR or LF)
            0x0D | 0x0A => {
                crate::println!();
                let line = core::str::from_utf8_unchecked(&LINE_BUF[..LINE_LEN]);
                execute(line);
                LINE_LEN = 0;
                crate::print!("{}", PROMPT);
            }
            // Backspace (DEL or BS)
            0x7F | 0x08 => {
                if LINE_LEN > 0 {
                    LINE_LEN -= 1;
                    // Erase character on terminal: backspace, space, backspace
                    crate::uart::putc(0x08);
                    crate::uart::putc(b' ');
                    crate::uart::putc(0x08);
                    // Mirror to framebuffer console
                    crate::console::putc(0x08);
                    crate::console::putc(b' ');
                    crate::console::putc(0x08);
                }
            }
            // Printable ASCII
            0x20..=0x7E => {
                if LINE_LEN < MAX_LINE {
                    LINE_BUF[LINE_LEN] = byte;
                    LINE_LEN += 1;
                    crate::uart::putc(byte);
                    // Mirror to framebuffer console
                    crate::console::putc(byte);
                }
            }
            // Ignore other bytes
            _ => {}
        }
    }
}

fn execute(line: &str) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }

    let mut parts = line.splitn(4, ' ');
    let cmd = parts.next().unwrap_or("");
    let arg1 = parts.next().unwrap_or("");
    let arg2 = parts.next().unwrap_or("");
    let arg3 = parts.next().unwrap_or("");

    match cmd {
        "help" => cmd_help(),
        "info" => cmd_info(),
        "mem" => cmd_mem(arg1, arg2),
        "poke" => cmd_poke(arg1, arg2),
        "timer" => cmd_timer(),
        "panic" => cmd_panic(),
        "fault" => cmd_fault(),
        "clear" => cmd_clear(arg1),
        "reboot" => cmd_reboot(),
        // Graph commands
        "graph" | "gs" => cmd_graph_stats(),
        "nodes" => cmd_nodes(),
        "node" => cmd_node(arg1),
        "mknode" => cmd_mknode(arg1, arg2),
        "edge" => cmd_edge(arg1, arg2, arg3),
        "set" => cmd_set(line),
        "cat" => cmd_cat(arg1),
        "walk" => cmd_walk(arg1),
        "find" => cmd_find(arg1),
        "rm" => cmd_rm(arg1),
        "render" => cmd_render(),
        "nav" => cmd_nav(),
        "status" => cmd_status(),
        "gql" => cmd_gql(line),
        "save" => cmd_save(),
        "load" => cmd_load(),
        "disk" => cmd_disk(),
        // Task commands
        "ps" => cmd_ps(),
        "spawn" => cmd_spawn(arg1),
        "kill" => cmd_kill(arg1),
        // IPC commands
        "ipc" => cmd_ipc(),
        "peek" => cmd_peek(arg1),
        // Scripting commands
        "run" => cmd_run(arg1),
        "edit" => cmd_edit(arg1),
        // Console commands
        "tty" | "console" => cmd_tty(),
        // Window manager
        "window" | "win" => cmd_window(arg1),
        "windows" => cmd_windows(),
        // DOOM
        "doom" => crate::doom::start(),
        // Network
        "ping" => cmd_ping(arg1),
        "net" | "netstat" => cmd_netstat(),
        "arp" => cmd_arp(arg1),
        "tcp" => cmd_tcp(arg1, arg2, arg3),
        "httpd" => cmd_httpd(arg1, arg2),
        "users" => cmd_users(),
        _ => {
            crate::println!("Unknown command: {}", cmd);
            crate::println!("Type 'help' for available commands.");
        }
    }
}

fn cmd_help() {
    crate::println!("Available commands:");
    crate::println!("  help          - show this help");
    crate::println!("  info          - system information");
    crate::println!("  mem <a> [n]   - hex dump memory");
    crate::println!("  poke <a> <v>  - write u32 to address");
    crate::println!("  timer         - show tick count & uptime");
    crate::println!("  panic         - trigger test panic");
    crate::println!("  fault         - trigger page fault");
    crate::println!("  clear         - clear screen");
    crate::println!("  reboot        - reboot via SBI");
    crate::println!("Graph commands:");
    crate::println!("  graph         - graph stats");
    crate::println!("  nodes         - list all nodes");
    crate::println!("  node <id>     - show node details");
    crate::println!("  mknode <t> <n>- create node (text/binary/config/system/dir/comp)");
    crate::println!("  edge <f> <l> <t> - add edge from node f to t");
    crate::println!("  set <id> ...  - set node content");
    crate::println!("  cat <id>      - show node content");
    crate::println!("  walk <id>     - walk node edges");
    crate::println!("  find <name>   - find nodes by name");
    crate::println!("  rm <id>       - remove a node");
    crate::println!("  render        - enter graph navigator (framebuffer)");
    crate::println!("  nav           - enter graph navigator (framebuffer)");
    crate::println!("  gql <query>   - graph query language (try: gql type=system)");
    crate::println!("  status        - live system overview");
    crate::println!("Disk commands:");
    crate::println!("  save          - save graph to disk");
    crate::println!("  load          - load graph from disk");
    crate::println!("  disk          - show disk info");
    crate::println!("Task commands:");
    crate::println!("  ps            - list all tasks with preemption stats");
    crate::println!("  spawn <name>  - spawn a demo task (counter, fibonacci, busyloop,");
    crate::println!("                  producer, consumer, pingpong, userdemo, baddemo,");
    crate::println!("                  who, explorer, editor, naughty [M30],");
    crate::println!("                  hello [M31 — native Rust on helios-std])");
    crate::println!("  spawn <id>    - spawn a USER-MODE task from a code node (M29)");
    crate::println!("  kill <id>     - kill a task by ID");
    crate::println!("IPC commands:");
    crate::println!("  ipc           - list all IPC channels");
    crate::println!("  peek <id>     - peek at a channel's messages");
    crate::println!("Scripting commands:");
    crate::println!("  run <id>      - execute a text node as a script");
    crate::println!("  edit <id>     - line editor for node content");
    crate::println!("Display commands:");
    crate::println!("  tty           - switch framebuffer to text console");
    crate::println!("  render | nav  - enter graph navigator");
    crate::println!("Window manager:");
    crate::println!("  window <id>   - toggle windowed mode for a node");
    crate::println!("  windows       - list all open windows");
    crate::println!("Network:");
    crate::println!("  net           - show net interface status");
    crate::println!("  ping <ip>     - ICMP echo to an IP (e.g. ping 10.0.2.2)");
    crate::println!("  arp [ip]      - show ARP cache, or resolve an IP");
    crate::println!("  tcp listen <port>          - open a TCP listener (blocks until key)");
    crate::println!("  tcp connect <ip> <port>    - TCP active open (sends 'hello\\n')");
    crate::println!("  tcp stats                  - show TCP state & stats");
    crate::println!("  httpd start [port]         - start HTTP server (reads+writes)");
    crate::println!("  httpd stop                 - stop HTTP server");
    crate::println!("  httpd stats                - show HTTP server stats");
    crate::println!("User nodes (POSTed via HTTP):");
    crate::println!("  users         - list externally-created nodes (id, origin IP, uptime)");
    crate::println!("  clear users   - delete all externally-created nodes");
}

fn cmd_info() {
    let satp = arch::read_satp();
    let ticks = trap::tick_count();
    let time = arch::read_time();
    let uptime_s = time / TIMER_FREQ;
    let uptime_frac = (time % TIMER_FREQ) / (TIMER_FREQ / 10);

    crate::println!("Helios v{}", env!("CARGO_PKG_VERSION"));
    crate::println!("SATP: {:#018x}", satp);
    crate::println!("Timer ticks: {}", ticks);
    crate::println!("Uptime: {}.{}s", uptime_s, uptime_frac);
}

fn cmd_timer() {
    let ticks = trap::tick_count();
    let time = arch::read_time();
    let uptime_s = time / TIMER_FREQ;
    let uptime_frac = (time % TIMER_FREQ) / (TIMER_FREQ / 10);

    crate::println!("Timer ticks: {}", ticks);
    crate::println!("Uptime: {}.{}s", uptime_s, uptime_frac);
}

fn parse_usize(s: &str) -> Option<usize> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        usize::from_str_radix(hex, 16).ok()
    } else {
        // Try decimal
        usize::from_str_radix(s, 10).ok()
    }
}

fn cmd_mem(addr_str: &str, count_str: &str) {
    let addr = match parse_usize(addr_str) {
        Some(a) => a,
        None => {
            crate::println!("Usage: mem <addr> [count]");
            return;
        }
    };
    let count = if count_str.is_empty() {
        64
    } else {
        match parse_usize(count_str) {
            Some(c) => c,
            None => {
                crate::println!("Invalid count");
                return;
            }
        }
    };

    // Limit to reasonable size
    let count = if count > 4096 { 4096 } else { count };

    let mut offset = 0usize;
    while offset < count {
        let line_addr = addr + offset;
        crate::print!("{:08x}: ", line_addr);

        // Hex bytes
        let line_len = if count - offset >= 16 { 16 } else { count - offset };
        for i in 0..16 {
            if i < line_len {
                let byte = unsafe { core::ptr::read_volatile((line_addr + i) as *const u8) };
                crate::print!("{:02x} ", byte);
            } else {
                crate::print!("   ");
            }
            if i == 7 {
                crate::print!(" ");
            }
        }

        // ASCII
        crate::print!(" ");
        for i in 0..line_len {
            let byte = unsafe { core::ptr::read_volatile((line_addr + i) as *const u8) };
            let ch = if byte >= 0x20 && byte <= 0x7E { byte } else { b'.' };
            crate::uart::putc(ch);
            crate::console::putc(ch);
        }
        crate::println!();

        offset += 16;
    }
}

fn cmd_poke(addr_str: &str, val_str: &str) {
    let addr = match parse_usize(addr_str) {
        Some(a) => a,
        None => {
            crate::println!("Usage: poke <addr> <value>");
            return;
        }
    };
    let val = match parse_usize(val_str) {
        Some(v) => v as u32,
        None => {
            crate::println!("Usage: poke <addr> <value>");
            return;
        }
    };

    unsafe {
        core::ptr::write_volatile(addr as *mut u32, val);
    }
    crate::println!("Wrote {:#010x} to {:#010x}", val, addr);
}

fn cmd_panic() {
    panic!("User-triggered test panic");
}

fn cmd_fault() {
    crate::println!("Triggering page fault (reading unmapped address 0xDEAD_0000)...");
    unsafe {
        let _val = core::ptr::read_volatile(0xDEAD_0000usize as *const u64);
    }
}

fn cmd_clear(arg: &str) {
    match arg.trim() {
        "" => {
            // ANSI escape: clear screen and move cursor home
            crate::print!("\x1b[2J\x1b[H");
        }
        "users" => cmd_clear_users(),
        other => {
            crate::println!("Unknown argument to 'clear': {}", other);
            crate::println!("Usage: clear           (clear screen)");
            crate::println!("       clear users    (remove all externally-created nodes)");
        }
    }
}

fn cmd_users() {
    let list = crate::graph::user::all();
    if list.is_empty() {
        crate::println!("user nodes: (none)");
        return;
    }
    crate::println!("user nodes:");
    let g = crate::graph::get();
    for (id, info) in list.iter() {
        let (ty, name) = match g.get_node(*id) {
            Some(n) => (alloc::format!("{}", n.type_tag), alloc::string::String::from(n.name.as_str())),
            None => (alloc::string::String::from("(gone)"), alloc::string::String::from("(gone)")),
        };
        crate::println!(
            "  #{} {} \"{}\" from {}.{}.{}.{} at uptime={}s",
            id,
            ty,
            name,
            info.source_ip[0],
            info.source_ip[1],
            info.source_ip[2],
            info.source_ip[3],
            info.created_uptime_s
        );
    }
}

fn cmd_clear_users() {
    let list = crate::graph::user::all();
    let n = list.len();
    if n == 0 {
        crate::println!("no user nodes to clear");
        return;
    }
    let g = crate::graph::get_mut();
    for (id, _) in list.iter() {
        g.remove_node(*id);
        crate::graph::user::forget(*id);
    }
    crate::println!("cleared {} user node(s)", n);
}

fn cmd_reboot() {
    crate::println!("Rebooting...");
    arch::sbi_reboot();
}

// ---------------------------------------------------------------------------
// Graph commands
// ---------------------------------------------------------------------------

fn parse_u64(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        u64::from_str_radix(s, 10).ok()
    }
}

fn cmd_graph_stats() {
    let g = crate::graph::get();
    crate::println!("Graph: {} nodes, {} edges", g.node_count(), g.edge_count());
}

fn cmd_nodes() {
    let g = crate::graph::get();
    crate::println!("{:>4}  {:<10} Name", "ID", "Type");
    for node in g.nodes.values() {
        crate::println!("{:>4}  {:<10} {}", node.id, node.type_tag, node.name);
    }
}

fn cmd_node(id_str: &str) {
    let id = match parse_u64(id_str) {
        Some(id) => id,
        None => {
            crate::println!("Usage: node <id>");
            return;
        }
    };
    crate::graph::live::refresh_system_nodes();
    let g = crate::graph::get();
    let node = match g.get_node(id) {
        Some(n) => n,
        None => {
            crate::println!("Node #{} not found", id);
            return;
        }
    };

    crate::println!("Node #{} \"{}\" ({})", node.id, node.name, node.type_tag);
    if node.type_tag == crate::graph::NodeType::Computed {
        let formula = core::str::from_utf8(&node.content).unwrap_or("(invalid)");
        crate::println!("  Formula: {}", formula);
        let result = node.display_content(g);
        crate::println!("  Result:  {}", result);
    } else if node.content.is_empty() {
        crate::println!("  Content: (empty)");
    } else {
        match core::str::from_utf8(&node.content) {
            Ok(s) => {
                if s.len() <= 80 {
                    crate::println!("  Content: \"{}\"", s);
                } else {
                    crate::println!("  Content: ({} bytes)", node.content.len());
                }
            }
            Err(_) => crate::println!("  Content: ({} bytes, binary)", node.content.len()),
        }
    }
    if node.edges.is_empty() {
        crate::println!("  Edges: (none)");
    } else {
        crate::println!("  Edges:");
        for edge in &node.edges {
            let target_name = g
                .get_node(edge.target)
                .map(|n| n.name.as_str())
                .unwrap_or("???");
            crate::println!("    --{}--> #{} \"{}\"", edge.label, edge.target, target_name);
        }
    }
}

fn cmd_mknode(type_str: &str, name_str: &str) {
    if type_str.is_empty() || name_str.is_empty() {
        crate::println!("Usage: mknode <type> <name>");
        crate::println!("Types: text, binary, config, system, dir, channel");
        return;
    }
    let type_tag = match crate::graph::NodeType::from_str(type_str) {
        Some(t) => t,
        None => {
            crate::println!("Unknown type '{}'. Use: text, binary, config, system, dir, comp", type_str);
            return;
        }
    };
    let g = crate::graph::get_mut();
    let id = g.create_node(type_tag, name_str);
    crate::println!("Created node #{} \"{}\" ({})", id, name_str, type_tag);
}

fn cmd_edge(from_str: &str, label: &str, to_str: &str) {
    let from = match parse_u64(from_str) {
        Some(v) => v,
        None => {
            crate::println!("Usage: edge <from_id> <label> <to_id>");
            return;
        }
    };
    let to = match parse_u64(to_str) {
        Some(v) => v,
        None => {
            crate::println!("Usage: edge <from_id> <label> <to_id>");
            return;
        }
    };
    if label.is_empty() {
        crate::println!("Usage: edge <from_id> <label> <to_id>");
        return;
    }
    let g = crate::graph::get_mut();
    if g.add_edge(from, label, to) {
        crate::println!("Edge added: #{} --{}--> #{}", from, label, to);
    } else {
        crate::println!("Failed: node #{} or #{} not found", from, to);
    }
}

fn cmd_set(line: &str) {
    // Parse: "set <id> <content...>"
    let rest = line.trim().strip_prefix("set").unwrap_or("").trim();
    let (id_str, content) = match rest.find(' ') {
        Some(pos) => (&rest[..pos], &rest[pos + 1..]),
        None => {
            crate::println!("Usage: set <id> <content...>");
            return;
        }
    };
    let id = match parse_u64(id_str) {
        Some(v) => v,
        None => {
            crate::println!("Usage: set <id> <content...>");
            return;
        }
    };
    let g = crate::graph::get_mut();
    match g.get_node_mut(id) {
        Some(node) => {
            let bytes = content.as_bytes();
            node.content = alloc::vec::Vec::from(bytes);
            crate::println!("Set content of node #{} ({} bytes)", id, bytes.len());
        }
        None => crate::println!("Node #{} not found", id),
    }
}

fn cmd_cat(id_str: &str) {
    let id = match parse_u64(id_str) {
        Some(v) => v,
        None => {
            crate::println!("Usage: cat <id>");
            return;
        }
    };
    crate::graph::live::refresh_system_nodes();
    let g = crate::graph::get();
    match g.get_node(id) {
        Some(node) => {
            if node.type_tag == crate::graph::NodeType::Computed {
                let result = node.display_content(g);
                crate::println!("{}", result);
            } else if node.content.is_empty() {
                crate::println!("(empty)");
            } else {
                match core::str::from_utf8(&node.content) {
                    Ok(s) => crate::println!("{}", s),
                    Err(_) => {
                        // Hex dump for non-UTF-8
                        for (i, byte) in node.content.iter().enumerate() {
                            crate::print!("{:02x} ", byte);
                            if (i + 1) % 16 == 0 {
                                crate::println!();
                            }
                        }
                        if node.content.len() % 16 != 0 {
                            crate::println!();
                        }
                    }
                }
            }
        }
        None => crate::println!("Node #{} not found", id),
    }
}

fn cmd_walk(id_str: &str) {
    let id = match parse_u64(id_str) {
        Some(v) => v,
        None => {
            crate::println!("Usage: walk <id>");
            return;
        }
    };
    crate::graph::live::refresh_system_nodes();
    let g = crate::graph::get();
    let node = match g.get_node(id) {
        Some(n) => n,
        None => {
            crate::println!("Node #{} not found", id);
            return;
        }
    };
    crate::println!("Node #{} \"{}\" ({})", node.id, node.name, node.type_tag);
    if node.edges.is_empty() {
        crate::println!("  (no edges)");
    } else {
        for edge in &node.edges {
            match g.get_node(edge.target) {
                Some(target) => {
                    crate::println!(
                        "  --{}--> #{} \"{}\" ({})",
                        edge.label,
                        target.id,
                        target.name,
                        target.type_tag
                    );
                }
                None => {
                    crate::println!("  --{}--> #{} (missing)", edge.label, edge.target);
                }
            }
        }
    }
}

fn cmd_find(name: &str) {
    if name.is_empty() {
        crate::println!("Usage: find <name>");
        return;
    }
    let g = crate::graph::get();
    let results = g.find_by_name(name);
    if results.is_empty() {
        crate::println!("No nodes matching '{}'", name);
    } else {
        for node in results {
            crate::println!("  #{}  {}  {}", node.id, node.type_tag, node.name);
        }
    }
}

fn cmd_rm(id_str: &str) {
    let id = match parse_u64(id_str) {
        Some(v) => v,
        None => {
            crate::println!("Usage: rm <id>");
            return;
        }
    };
    let g = crate::graph::get_mut();
    // Get name before removal for display
    let name = g.get_node(id).map(|n| alloc::string::String::from(n.name.as_str()));
    if g.remove_node(id) {
        crate::println!("Removed node #{} \"{}\"", id, name.unwrap_or_default());
    } else {
        crate::println!("Node #{} not found", id);
    }
}

fn cmd_render() {
    if crate::framebuffer::get().is_some() {
        enter_nav_mode();
    } else {
        crate::println!("No framebuffer available (UART-only mode).");
    }
}

fn cmd_nav() {
    if crate::framebuffer::get().is_some() {
        enter_nav_mode();
    } else {
        crate::println!("No framebuffer available (UART-only mode).");
    }
}

fn cmd_gql(line: &str) {
    let rest = line.trim().strip_prefix("gql").unwrap_or("").trim();
    if rest.is_empty() {
        crate::println!("Usage: gql <query>");
        crate::println!("  gql type=system      - filter by type");
        crate::println!("  gql name~mem         - name substring match");
        crate::println!("  gql children 1       - children of node 1");
        crate::println!("  gql path 1 5         - shortest path");
        crate::println!("  gql count            - total node count");
        return;
    }
    crate::graph::live::refresh_system_nodes();
    let g = crate::graph::get();
    crate::graph::query::execute(rest, g);
}

fn cmd_save() {
    let blk = match crate::virtio::blk::get_mut() {
        Some(b) => b,
        None => {
            crate::println!("No disk device available.");
            return;
        }
    };

    let g = crate::graph::get();
    let data = crate::graph::persist::serialize(g);
    let data_len = data.len();

    // Prepend length as u64 (8 bytes), then the serialized data
    let total_len = 8 + data_len;
    let sectors = (total_len + 511) / 512;

    let mut payload = alloc::vec![0u8; sectors * 512];
    // Write length
    payload[0..8].copy_from_slice(&(data_len as u64).to_le_bytes());
    // Write data
    payload[8..8 + data_len].copy_from_slice(&data);

    if blk.write(0, &payload) {
        crate::println!(
            "Graph saved to disk ({} bytes, {} sector{})",
            data_len,
            sectors,
            if sectors == 1 { "" } else { "s" }
        );
    } else {
        crate::println!("Failed to write graph to disk!");
    }
}

fn cmd_load() {
    let blk = match crate::virtio::blk::get_mut() {
        Some(b) => b,
        None => {
            crate::println!("No disk device available.");
            return;
        }
    };

    // Read first sector to get the length
    let mut header_sector = [0u8; 512];
    if !blk.read_sector(0, &mut header_sector) {
        crate::println!("Failed to read disk!");
        return;
    }

    let data_len = u64::from_le_bytes([
        header_sector[0], header_sector[1], header_sector[2], header_sector[3],
        header_sector[4], header_sector[5], header_sector[6], header_sector[7],
    ]) as usize;

    if data_len == 0 || data_len > 16 * 1024 * 1024 {
        crate::println!("No valid graph data on disk.");
        return;
    }

    // Read all needed sectors
    let total_len = 8 + data_len;
    let sectors = (total_len + 511) / 512;
    let mut payload = alloc::vec![0u8; sectors * 512];

    if !blk.read(0, &mut payload) {
        crate::println!("Failed to read graph data from disk!");
        return;
    }

    let data = &payload[8..8 + data_len];
    match crate::graph::persist::deserialize(data) {
        Some(graph) => {
            let nodes = graph.node_count();
            let edges = graph.edge_count();
            crate::graph::replace(graph);
            crate::println!(
                "Graph loaded from disk ({} bytes, {} nodes, {} edges)",
                data_len, nodes, edges
            );
        }
        None => {
            crate::println!("Failed to deserialize graph data!");
        }
    }
}

fn cmd_disk() {
    if !crate::virtio::blk::is_present() {
        crate::println!("Disk: not present");
        return;
    }

    crate::println!("Disk: present (virtio-blk)");

    // Try to read sector 0 to see if there's saved data
    let blk = match crate::virtio::blk::get_mut() {
        Some(b) => b,
        None => return,
    };

    let mut header_sector = [0u8; 512];
    if blk.read_sector(0, &mut header_sector) {
        let data_len = u64::from_le_bytes([
            header_sector[0], header_sector[1], header_sector[2], header_sector[3],
            header_sector[4], header_sector[5], header_sector[6], header_sector[7],
        ]) as usize;

        if data_len > 0 && data_len < 16 * 1024 * 1024 {
            let sectors = (8 + data_len + 511) / 512;
            crate::println!("Saved data: {} bytes ({} sectors)", data_len, sectors);
        } else {
            crate::println!("No saved data on disk.");
        }
    } else {
        crate::println!("Could not read disk.");
    }
}

fn cmd_status() {
    let time = arch::read_time();
    let uptime_s = time / TIMER_FREQ;
    let uptime_frac = (time % TIMER_FREQ) / (TIMER_FREQ / 10);
    let ticks = trap::tick_count();
    let used_kib = crate::alloc_impl::heap_used() / 1024;
    let free_kib = crate::alloc_impl::heap_free() / 1024;
    let total_kib = crate::alloc_impl::heap_total() / 1024;
    let g = crate::graph::get();
    let nodes = g.node_count();
    let edges = g.edge_count();

    crate::println!(
        "Helios v{} | Hart 0 | Uptime: {}.{}s",
        env!("CARGO_PKG_VERSION"),
        uptime_s,
        uptime_frac
    );
    crate::println!("Memory: ~{} KiB used / {} KiB free / {} KiB heap", used_kib, free_kib, total_kib);
    crate::println!("Graph: {} nodes, {} edges", nodes, edges);
    crate::println!("Timer: {} ticks @ 10 MHz", ticks);
}

// ---------------------------------------------------------------------------
// Scripting commands
// ---------------------------------------------------------------------------

fn cmd_run(id_str: &str) {
    let id = match parse_u64(id_str) {
        Some(v) => v,
        None => {
            crate::println!("Usage: run <node_id>");
            return;
        }
    };

    // Prevent recursive script execution
    unsafe {
        if RUNNING_SCRIPT {
            crate::println!("Error: cannot nest 'run' — script already executing");
            return;
        }
    }

    // Read the node content
    let content = {
        let g = crate::graph::get();
        match g.get_node(id) {
            Some(node) => {
                match core::str::from_utf8(&node.content) {
                    Ok(s) => alloc::string::String::from(s),
                    Err(_) => {
                        crate::println!("Node #{} content is not valid UTF-8", id);
                        return;
                    }
                }
            }
            None => {
                crate::println!("Node #{} not found", id);
                return;
            }
        }
    };

    if content.is_empty() {
        crate::println!("Node #{} is empty — nothing to execute", id);
        return;
    }

    unsafe { RUNNING_SCRIPT = true; }

    let mut cmd_count: usize = 0;
    for line in content.split('\n') {
        let line = line.trim();
        // Skip blank lines and comments
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        crate::println!("> {}", line);
        execute(line);
        cmd_count += 1;
    }

    unsafe { RUNNING_SCRIPT = false; }

    crate::println!("Script #{}: {} commands executed", id, cmd_count);
}

fn cmd_edit(id_str: &str) {
    let id = match parse_u64(id_str) {
        Some(v) => v,
        None => {
            crate::println!("Usage: edit <node_id>");
            return;
        }
    };

    // Verify the node exists
    let name = {
        let g = crate::graph::get();
        match g.get_node(id) {
            Some(node) => alloc::string::String::from(node.name.as_str()),
            None => {
                crate::println!("Node #{} not found", id);
                return;
            }
        }
    };

    crate::println!("Editing node #{} \"{}\". Enter lines, empty line to finish.", id, name);
    crate::print!("{}", EDIT_PROMPT);

    unsafe {
        EDIT_MODE = true;
        EDIT_NODE_ID = id;
        EDIT_BUFFER = Some(alloc::vec::Vec::new());
    }
}

// ---------------------------------------------------------------------------
// Task commands
// ---------------------------------------------------------------------------

fn cmd_ps() {
    let tasks = crate::task::list();
    crate::println!("{:>4}  {:<16} {:<10} Preemptions", "ID", "Name", "State");
    for (id, name, state, preemptions) in &tasks {
        crate::println!("{:>4}  {:<16} {:<10} {}", id, name, state, preemptions);
    }
}

fn cmd_spawn(name: &str) {
    if name.is_empty() {
        crate::println!("Usage: spawn <name|node_id>");
        crate::println!("  Kernel demos: counter, fibonacci, busyloop, producer, consumer, pingpong");
        crate::println!("  User space:   spawn <code_node_id>  (drops to U-mode with edge-based caps)");
        crate::println!("                spawn userdemo  (M29: read/forbidden demo)");
        crate::println!("                spawn baddemo   (M29: MMU page-fault demo)");
        crate::println!("                spawn who       (M30: SYS_SELF)");
        crate::println!("                spawn explorer  (M30: SYS_LIST_EDGES on self)");
        crate::println!("                spawn editor    (M30: SYS_WRITE_NODE on scratch)");
        crate::println!("                spawn naughty   (M30: SYS_WRITE_NODE -> EPERM)");
        crate::println!("                spawn hello     (M31: native Rust on helios-std)");
        return;
    }
    // Shortcut: "spawn userdemo" launches the boot-time demo code node.
    if name == "userdemo" {
        let code_id = crate::user::demo_code_id();
        let text_id = crate::user::demo_text_id();
        if code_id == 0 {
            crate::println!("user-demo-code not initialized");
            return;
        }
        crate::println!("helios> spawning M29 user-space demo (code #{}, read #{}, forbidden #1)", code_id, text_id);
        let rc = crate::user::run_user_task_from_code_node(code_id, text_id, 1);
        crate::println!("user task returned {}", rc);
        return;
    }
    // Shortcut: "spawn baddemo" launches the MMU-violation test (task is
    // killed by a page fault rather than exiting via SYS_EXIT).
    if name == "baddemo" {
        let code_id = crate::user::baddemo_code_id();
        let text_id = crate::user::demo_text_id();
        if code_id == 0 {
            crate::println!("user-baddemo-code not initialized");
            return;
        }
        crate::println!("helios> spawning baddemo — expect MMU page fault and task kill");
        let rc = crate::user::run_user_task_from_code_node(code_id, text_id, 1);
        crate::println!("user task returned {}", rc);
        return;
    }
    // M30 demos -------------------------------------------------------
    if name == "who" {
        let code_id = crate::user::who_code_id();
        if code_id == 0 { crate::println!("user-who-code not initialized"); return; }
        crate::println!("helios> spawning M30 'who' demo (SYS_SELF + SYS_PRINT)");
        // Self-traverse not needed (no list/follow), but harmless to omit.
        let rc = crate::user::run_user_task_with_caps(code_id, &[], false, 0, 0);
        crate::println!("user task returned {}", rc);
        return;
    }
    if name == "explorer" {
        let code_id = crate::user::explorer_code_id();
        if code_id == 0 { crate::println!("user-explorer-code not initialized"); return; }
        crate::println!("helios> spawning M30 'explorer' demo (SYS_LIST_EDGES on self)");
        // Needs traverse cap to self to enumerate its own edges.
        let rc = crate::user::run_user_task_with_caps(code_id, &[], true, 0, 0);
        crate::println!("user task returned {}", rc);
        return;
    }
    if name == "editor" {
        let code_id = crate::user::editor_code_id();
        let scratch = crate::user::scratch_id();
        if code_id == 0 || scratch == 0 {
            crate::println!("user-editor-code or scratch not initialized");
            return;
        }
        crate::println!("helios> spawning M30 'editor' demo (read+write on scratch #{})", scratch);
        let rc = crate::user::run_user_task_with_caps(
            code_id,
            &[("read", scratch), ("write", scratch)],
            false,
            scratch as usize,
            0,
        );
        crate::println!("user task returned {}", rc);
        return;
    }
    if name == "naughty" {
        let code_id = crate::user::naughty_code_id();
        let scratch = crate::user::scratch_id();
        if code_id == 0 || scratch == 0 {
            crate::println!("user-naughty-code or scratch not initialized");
            return;
        }
        crate::println!("helios> spawning M30 'naughty' demo (read-only on scratch #{}; write will be refused)", scratch);
        let rc = crate::user::run_user_task_with_caps(
            code_id,
            &[("read", scratch)], // NO write edge — sys_write_node will return EPERM
            false,
            scratch as usize,
            0,
        );
        crate::println!("user task returned {}", rc);
        return;
    }
    // M31: spawn the native Rust hello program built against helios-std.
    //
    // Each spawn creates a fresh task node with:
    //   - `exec` edge to the compiled hello-user code binary
    //   - `traverse` edge back to itself, so `helios_std::graph::list_edges(me)` works
    // and drops to U-mode at 0x4000_0000 (the first byte of the binary,
    // which is the `_start` shim the `helios_entry!` macro expands to).
    if name == "hello" || name == "hello-user" || name == "rustdemo" {
        let code_id = crate::user::hello_code_id();
        if code_id == 0 {
            crate::println!("hello-user-code not initialized");
            return;
        }
        crate::println!(
            "helios> spawning M31 'hello' — native Rust on helios-std (code #{})",
            code_id,
        );
        let rc = crate::user::run_user_task_with_caps(code_id, &[], true, 0, 0);
        crate::println!("user task returned {}", rc);
        return;
    }
    // Numeric argument -> treat as a code node id and launch as user task.
    if let Some(id) = parse_usize(name) {
        let code_id = id as u64;
        // Use text demo id as the read target; forbidden = root (node 1).
        let text_id = crate::user::demo_text_id();
        crate::println!("helios> spawning user task from code node #{} (read #{}, forbidden #1)", code_id, text_id);
        let rc = crate::user::run_user_task_from_code_node(code_id, text_id, 1);
        crate::println!("user task returned {}", rc);
        return;
    }
    match name {
        "pingpong" => {
            // Special case: spawns two tasks
            crate::task::spawn_pingpong();
            return;
        }
        _ => {}
    }
    let f: fn() = match name {
        "counter" => crate::task::demo_counter,
        "fibonacci" => crate::task::demo_fibonacci,
        "busyloop" => crate::task::demo_busyloop,
        "producer" => crate::task::demo_producer,
        "consumer" => crate::task::demo_consumer,
        _ => {
            crate::println!("Unknown task '{}'. Available: counter, fibonacci, busyloop, producer, consumer, pingpong, userdemo, baddemo, who, explorer, editor, naughty, hello (M31), or a numeric code node id", name);
            return;
        }
    };
    let id = crate::task::spawn(name, f);
    crate::println!("Spawned task #{} \"{}\"", id, name);
}

fn cmd_kill(id_str: &str) {
    let id = match parse_usize(id_str) {
        Some(v) => v,
        None => {
            crate::println!("Usage: kill <id>");
            return;
        }
    };
    if id == 0 {
        crate::println!("Cannot kill the shell task.");
        return;
    }
    if crate::task::kill(id) {
        crate::println!("Killed task #{}", id);
    } else {
        crate::println!("Task #{} not found or already done", id);
    }
}

// ---------------------------------------------------------------------------
// IPC commands
// ---------------------------------------------------------------------------

fn cmd_tty() {
    if crate::framebuffer::get().is_some() {
        crate::console::set_active(true);
        crate::println!("Framebuffer console active. Type 'render' for graph view.");
    } else {
        crate::println!("No framebuffer available (UART-only mode).");
    }
}

fn cmd_ipc() {
    let channels = crate::ipc::list_channels();
    if channels.is_empty() {
        crate::println!("No IPC channels. Use 'spawn producer' or 'spawn pingpong' to create some.");
        return;
    }
    crate::println!("{:>4}  {:<16} {:<6} Content", "ID", "Name", "Msgs");
    for (id, name, msgs, preview) in &channels {
        let display = if preview.is_empty() {
            alloc::string::String::from("(empty)")
        } else {
            preview.replace('\n', " | ")
        };
        crate::println!("{:>4}  {:<16} {:<6} {}", id, name, msgs, display);
    }
}

// ---------------------------------------------------------------------------
// Window manager commands
// ---------------------------------------------------------------------------

fn cmd_window(id_str: &str) {
    let id = match parse_u64(id_str) {
        Some(v) => v,
        None => {
            crate::println!("Usage: window <node_id>");
            return;
        }
    };

    // Verify the node exists before windowizing.
    {
        let g = crate::graph::get();
        if g.get_node(id).is_none() {
            crate::println!("Node #{} not found", id);
            return;
        }
    }

    // Place new windows at staggered positions so they don't stack up.
    let wm = crate::graph::window::get_mut();
    let count = wm.windows.len() as i32;
    let base_x = 80 + (count * 30).rem_euclid(400);
    let base_y = 150 + (count * 30).rem_euclid(300);

    let opened = crate::graph::window::toggle_window(id, base_x, base_y);
    if opened {
        crate::println!("Windowized node #{}", id);
    } else {
        crate::println!("Closed window for node #{}", id);
    }

    // Refresh the navigator if we are in it.
    if crate::framebuffer::get().is_some() {
        crate::graph::live::refresh_system_nodes();
        crate::graph::navigator::render_nav();
    }
}

fn cmd_windows() {
    let wm = crate::graph::window::get();
    if wm.windows.is_empty() {
        crate::println!("No windows open.");
        return;
    }
    let g = crate::graph::get();
    crate::println!("{:>4}  {:<16} {:<9} {:>4} {:>4} {:>4} {:>4} {:>3} {}",
        "node", "name", "state", "x", "y", "w", "h", "z", "");
    // Print in z-order (ascending; focused window is last/highest).
    let mut indices: alloc::vec::Vec<usize> = (0..wm.windows.len()).collect();
    indices.sort_by_key(|&i| wm.windows[i].z);
    for i in indices {
        let w = &wm.windows[i];
        let focused = wm.focused == Some(w.node_id);
        let name = g.get_node(w.node_id).map(|n| n.name.as_str()).unwrap_or("?");
        crate::println!(
            "{:>4}  {:<16} {:<9} {:>4} {:>4} {:>4} {:>4} {:>3} {}",
            w.node_id,
            name,
            if focused { "focused" } else { "bg" },
            w.x, w.y, w.w, w.h, w.z,
            if focused { "*" } else { "" }
        );
    }
}

fn cmd_peek(id_str: &str) {
    let id = match parse_u64(id_str) {
        Some(v) => v,
        None => {
            crate::println!("Usage: peek <channel_id>");
            return;
        }
    };
    match crate::ipc::peek(id) {
        Some(msg) => crate::println!("Channel #{} next message: {}", id, msg),
        None => crate::println!("Channel #{}: empty (or not a channel)", id),
    }
}

// ─── Network commands ───────────────────────────────────────────────────────

fn parse_ip(s: &str) -> Option<[u8; 4]> {
    let mut octets = [0u8; 4];
    let mut i = 0;
    for part in s.split('.') {
        if i >= 4 { return None; }
        octets[i] = part.parse::<u8>().ok()?;
        i += 1;
    }
    if i == 4 { Some(octets) } else { None }
}

fn cmd_netstat() {
    if !crate::virtio::net::is_present() {
        crate::println!("No network device.");
        return;
    }
    crate::net::update_graph_node();
    let mac = crate::net::our_mac();
    let s = crate::net::stats();
    crate::println!("net0: VirtIO network");
    crate::println!("  MAC:     {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);
    crate::println!("  IP:      {}.{}.{}.{}",
        crate::net::OUR_IP[0], crate::net::OUR_IP[1],
        crate::net::OUR_IP[2], crate::net::OUR_IP[3]);
    crate::println!("  Gateway: {}.{}.{}.{}",
        crate::net::GATEWAY_IP[0], crate::net::GATEWAY_IP[1],
        crate::net::GATEWAY_IP[2], crate::net::GATEWAY_IP[3]);
    crate::println!("  Netmask: {}.{}.{}.{}",
        crate::net::NETMASK[0], crate::net::NETMASK[1],
        crate::net::NETMASK[2], crate::net::NETMASK[3]);
    crate::println!("  RX: {} frames", s.rx_frames);
    crate::println!("  TX: {} frames", s.tx_frames);
    crate::println!("  ARP rx/tx: {}/{}", s.arp_rx, s.arp_tx);
    crate::println!("  ICMP rx/tx: {}/{}", s.icmp_rx, s.icmp_tx);
}

fn cmd_arp(arg: &str) {
    if !crate::virtio::net::is_present() {
        crate::println!("No network device.");
        return;
    }
    if arg.is_empty() {
        crate::println!("ARP cache:");
        for i in 0..8 {
            unsafe {
                let e = &crate::net::ARP_CACHE[i];
                if e.valid {
                    crate::println!("  {}.{}.{}.{} -> {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                        e.ip[0], e.ip[1], e.ip[2], e.ip[3],
                        e.mac[0], e.mac[1], e.mac[2], e.mac[3], e.mac[4], e.mac[5]);
                }
            }
        }
    } else {
        let ip = match parse_ip(arg) {
            Some(i) => i,
            None => { crate::println!("Usage: arp [a.b.c.d]"); return; }
        };
        if let Some(mac) = crate::net::arp_lookup(&ip) {
            crate::println!("  {}.{}.{}.{} -> {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} (cached)",
                ip[0], ip[1], ip[2], ip[3],
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);
            return;
        }
        crate::println!("Sending ARP request for {}.{}.{}.{}...", ip[0], ip[1], ip[2], ip[3]);
        crate::net::arp::send_request(ip);
        // Wait up to 2 seconds for reply
        let start = crate::arch::riscv64::read_time();
        let deadline = start + 2 * TIMER_FREQ;
        while crate::arch::riscv64::read_time() < deadline {
            crate::virtio::net::poll();
            if let Some(mac) = crate::net::arp_lookup(&ip) {
                crate::println!("  {}.{}.{}.{} -> {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                    ip[0], ip[1], ip[2], ip[3],
                    mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);
                return;
            }
            core::hint::spin_loop();
        }
        crate::println!("ARP timed out.");
    }
}

fn cmd_ping(arg: &str) {
    if !crate::virtio::net::is_present() {
        crate::println!("No network device.");
        return;
    }
    let target_ip = match parse_ip(arg) {
        Some(i) => i,
        None => { crate::println!("Usage: ping <a.b.c.d>"); return; }
    };

    // Resolve the next-hop MAC. For destinations outside our /24, use gateway.
    let same_subnet = target_ip[0] == crate::net::OUR_IP[0]
        && target_ip[1] == crate::net::OUR_IP[1]
        && target_ip[2] == crate::net::OUR_IP[2];
    let next_hop_ip = if same_subnet { target_ip } else { crate::net::GATEWAY_IP };

    let dst_mac = match crate::net::arp_lookup(&next_hop_ip) {
        Some(m) => m,
        None => {
            crate::println!("ARP: resolving {}.{}.{}.{}...",
                next_hop_ip[0], next_hop_ip[1], next_hop_ip[2], next_hop_ip[3]);
            crate::net::arp::send_request(next_hop_ip);
            // Wait up to 2 seconds.
            let start = crate::arch::riscv64::read_time();
            let deadline = start + 2 * TIMER_FREQ;
            loop {
                if crate::arch::riscv64::read_time() >= deadline {
                    crate::println!("ARP timeout — cannot resolve next-hop.");
                    return;
                }
                crate::virtio::net::poll();
                if let Some(m) = crate::net::arp_lookup(&next_hop_ip) {
                    break m;
                }
                core::hint::spin_loop();
            }
        }
    };

    // Send 4 ICMP echo requests, waiting up to 2s each.
    let ident = 0xB001u16;
    crate::println!("PING {}.{}.{}.{} (via {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}): 56 data bytes",
        target_ip[0], target_ip[1], target_ip[2], target_ip[3],
        dst_mac[0], dst_mac[1], dst_mac[2], dst_mac[3], dst_mac[4], dst_mac[5]);

    let mut sent = 0u32;
    let mut recv = 0u32;
    for seq in 0..4u16 {
        {
            let st = crate::net::stats_mut();
            st.ping_outstanding = true;
            st.ping_seq = seq;
            st.ping_ident = ident;
            st.ping_target_ip = target_ip;
            st.ping_reply_us = None;
        }
        if !crate::net::icmp::send_echo_request(target_ip, dst_mac, ident, seq) {
            crate::println!("send failed");
            return;
        }
        sent += 1;

        // Wait up to 2 seconds.
        let start = crate::arch::riscv64::read_time();
        let deadline = start + 2 * TIMER_FREQ;
        let mut got_reply = false;
        while crate::arch::riscv64::read_time() < deadline {
            crate::virtio::net::poll();
            let st = crate::net::stats();
            if !st.ping_outstanding {
                let us = st.ping_reply_us.unwrap_or(0);
                let ms = us / 1000;
                let frac = (us % 1000) / 10; // two digits of fractional ms
                crate::println!("64 bytes from {}.{}.{}.{}: icmp_seq={} ttl=64 time={}.{:02} ms",
                    target_ip[0], target_ip[1], target_ip[2], target_ip[3],
                    seq, ms, frac);
                recv += 1;
                got_reply = true;
                break;
            }
            core::hint::spin_loop();
        }
        if !got_reply {
            crate::println!("Request timeout for icmp_seq={}", seq);
            // Clear outstanding state
            crate::net::stats_mut().ping_outstanding = false;
        }

        // Short delay between pings (~0.3s)
        let pause_start = crate::arch::riscv64::read_time();
        while crate::arch::riscv64::read_time() - pause_start < TIMER_FREQ / 3 {
            crate::virtio::net::poll();
            core::hint::spin_loop();
        }
    }

    crate::println!("--- {}.{}.{}.{} ping statistics ---",
        target_ip[0], target_ip[1], target_ip[2], target_ip[3]);
    let loss = if sent > 0 { ((sent - recv) * 100) / sent } else { 0 };
    crate::println!("{} packets transmitted, {} received, {}% packet loss",
        sent, recv, loss);
}

// ─── TCP commands ───────────────────────────────────────────────────────────

fn cmd_tcp(sub: &str, arg1: &str, arg2: &str) {
    match sub {
        "listen" => cmd_tcp_listen(arg1),
        "connect" => cmd_tcp_connect(arg1, arg2),
        "stats" => cmd_tcp_stats(),
        "" => {
            crate::println!("Usage:");
            crate::println!("  tcp listen <port>");
            crate::println!("  tcp connect <a.b.c.d> <port>");
            crate::println!("  tcp stats");
        }
        _ => {
            crate::println!("Unknown tcp subcommand: {}", sub);
            crate::println!("Try: tcp listen <port> | tcp connect <ip> <port> | tcp stats");
        }
    }
}

fn cmd_tcp_listen(port_str: &str) {
    if !crate::virtio::net::is_present() {
        crate::println!("No network device.");
        return;
    }
    let port = match port_str.parse::<u16>() {
        Ok(p) if p > 0 => p,
        _ => {
            crate::println!("Usage: tcp listen <port>");
            return;
        }
    };
    let handle = match crate::net::tcp::tcp_listen(port) {
        Some(h) => h,
        None => {
            crate::println!("tcp: could not listen on {} (port in use or table full)", port);
            return;
        }
    };
    crate::println!("listening on port {}. Press any key to stop.", port);

    // Track active accepted sockets so we can drain data.
    let mut active: alloc::vec::Vec<usize> = alloc::vec::Vec::new();
    let mut rxbuf = [0u8; 1024];

    loop {
        // Check for user keypress — exits the listen loop.
        if let Some(byte) = crate::uart::getc() {
            let _ = byte;
            break;
        }

        // Poll net
        crate::virtio::net::poll();
        crate::net::tcp::tick();

        // Accept new connections.
        while let Some(sock_idx) = crate::net::tcp::accept(handle) {
            if let Some((ip, port_r)) = crate::net::tcp::socket_peer(sock_idx) {
                crate::println!(
                    "[accept] new connection from {}.{}.{}.{}:{} (socket #{})",
                    ip[0], ip[1], ip[2], ip[3], port_r, sock_idx
                );
            }
            active.push(sock_idx);
        }

        // Drain data from active sockets.
        let mut i = 0;
        while i < active.len() {
            let sock_idx = active[i];
            let state = crate::net::tcp::socket_state(sock_idx);
            let mut keep = true;
            match crate::net::tcp::recv(sock_idx, &mut rxbuf) {
                Some(0) => {
                    crate::println!("[close] socket #{} closed by peer", sock_idx);
                    crate::net::tcp::close(sock_idx);
                    keep = false;
                }
                Some(n) => {
                    let s = match core::str::from_utf8(&rxbuf[..n]) {
                        Ok(s) => alloc::string::String::from(s),
                        Err(_) => alloc::format!("<{} bytes binary>", n),
                    };
                    crate::println!("[recv sock #{}] {:?}", sock_idx, s);
                    // Echo back + a newline for trivial test
                    let echo = alloc::format!("helios-echo: {}", s);
                    let sent = crate::net::tcp::send(sock_idx, echo.as_bytes());
                    let _ = sent;
                    // Update graph node
                    crate::net::tcp::update_socket_node(sock_idx);
                }
                None => {
                    // No data. Check if the socket was RST/freed.
                    if state.is_none() {
                        keep = false;
                    }
                }
            }
            if !keep {
                active.swap_remove(i);
            } else {
                i += 1;
            }
        }

        core::hint::spin_loop();
    }

    // Close everything cleanly.
    for sock_idx in &active {
        crate::net::tcp::close(*sock_idx);
    }
    // Pump briefly to flush FINs.
    let flush_start = crate::arch::riscv64::read_time();
    while crate::arch::riscv64::read_time() - flush_start < TIMER_FREQ / 2 {
        crate::virtio::net::poll();
        crate::net::tcp::tick();
        core::hint::spin_loop();
    }
    crate::net::tcp::tcp_unlisten(port);
    crate::println!("stopped listening on port {}.", port);
}

fn cmd_tcp_connect(ip_str: &str, port_str: &str) {
    if !crate::virtio::net::is_present() {
        crate::println!("No network device.");
        return;
    }
    let ip = match parse_ip(ip_str) {
        Some(i) => i,
        None => { crate::println!("Usage: tcp connect <a.b.c.d> <port>"); return; }
    };
    let port = match port_str.parse::<u16>() {
        Ok(p) if p > 0 => p,
        _ => { crate::println!("Usage: tcp connect <a.b.c.d> <port>"); return; }
    };
    crate::println!("connecting to {}.{}.{}.{}:{} ...",
        ip[0], ip[1], ip[2], ip[3], port);
    let handle = match crate::net::tcp::tcp_connect(ip, port) {
        Some(h) => h,
        None => { crate::println!("connect: no ARP / no free socket"); return; }
    };
    // Wait up to ~3s for ESTABLISHED.
    let start = crate::arch::riscv64::read_time();
    let deadline = start + 3 * TIMER_FREQ;
    let mut established = false;
    while crate::arch::riscv64::read_time() < deadline {
        crate::virtio::net::poll();
        crate::net::tcp::tick();
        match crate::net::tcp::socket_state(handle) {
            Some(crate::net::tcp::State::Established) => { established = true; break; }
            None => break,
            _ => {}
        }
        core::hint::spin_loop();
    }
    if !established {
        crate::println!("connect: timed out / reset");
        crate::net::tcp::close(handle);
        return;
    }
    crate::println!("connected (socket #{}). sending 'hello\\n'", handle);
    crate::net::tcp::send(handle, b"hello\n");

    // Poll for reply / activity ~2s.
    let start = crate::arch::riscv64::read_time();
    let deadline = start + 2 * TIMER_FREQ;
    let mut buf = [0u8; 512];
    while crate::arch::riscv64::read_time() < deadline {
        crate::virtio::net::poll();
        crate::net::tcp::tick();
        if let Some(n) = crate::net::tcp::recv(handle, &mut buf) {
            if n > 0 {
                let s = match core::str::from_utf8(&buf[..n]) {
                    Ok(s) => alloc::string::String::from(s),
                    Err(_) => alloc::format!("<{} bytes>", n),
                };
                crate::println!("recv: {:?}", s);
            } else {
                crate::println!("peer closed");
                break;
            }
        }
        core::hint::spin_loop();
    }
    crate::net::tcp::close(handle);
    crate::println!("closed.");
}

fn cmd_tcp_stats() {
    let s = crate::net::tcp::stats();
    crate::println!("TCP stats:");
    crate::println!("  RX segments: {}   TX segments: {}", s.rx_segments, s.tx_segments);
    crate::println!("  RX bytes:    {}   TX bytes:    {}", s.rx_bytes, s.tx_bytes);
    crate::println!("  RST rx/tx: {}/{}  retransmits: {}  accepts: {}  closes: {}",
        s.resets_rx, s.resets_tx, s.retransmits, s.accepts, s.closes);
    crate::println!("Listeners:");
    let mut any = false;
    crate::net::tcp::each_listener(|i, l| {
        any = true;
        crate::println!("  [{}] port {}  backlog={}", i, l.port, l.backlog.len());
    });
    if !any { crate::println!("  (none)"); }
    crate::println!("Sockets:");
    any = false;
    crate::net::tcp::each_socket(|i, s| {
        any = true;
        crate::println!(
            "  [{}] {}:{} <-> {}.{}.{}.{}:{}  {}  rx={} tx={}",
            i, "local", s.local_port,
            s.remote_ip[0], s.remote_ip[1], s.remote_ip[2], s.remote_ip[3],
            s.remote_port,
            s.state.as_str(),
            s.rx_bytes, s.tx_bytes
        );
    });
    if !any { crate::println!("  (none)"); }
}

// ─── HTTP server commands ───────────────────────────────────────────────────

fn cmd_httpd(sub: &str, arg1: &str) {
    match sub {
        "start" => cmd_httpd_start(arg1),
        "stop" => cmd_httpd_stop(),
        "stats" => cmd_httpd_stats(),
        "" => {
            crate::println!("Usage:");
            crate::println!("  httpd start [port]     - start HTTP server (default port 80)");
            crate::println!("  httpd stop             - stop HTTP server");
            crate::println!("  httpd stats            - show request / byte counters");
        }
        _ => {
            crate::println!("Unknown httpd subcommand: {}", sub);
            crate::println!("Try: httpd start [port] | httpd stop | httpd stats");
        }
    }
}

fn cmd_httpd_start(port_str: &str) {
    if !crate::virtio::net::is_present() {
        crate::println!("No network device.");
        return;
    }
    let port: u16 = if port_str.is_empty() {
        80
    } else {
        match port_str.parse::<u16>() {
            Ok(p) if p > 0 => p,
            _ => {
                crate::println!("Usage: httpd start [port]");
                return;
            }
        }
    };
    if crate::net::http::is_running() {
        let p = crate::net::http::server_port().unwrap_or(0);
        crate::println!("httpd already running on port {}", p);
        return;
    }
    if crate::net::http::start(port) {
        crate::println!("httpd: serving graph as JSON on port {} (non-blocking)", port);
        crate::println!("       endpoints: / /ping /stats /nodes /nodes/{{id}} /tree");
        crate::println!("       shell remains responsive; type 'httpd stats' to see counters");
    } else {
        crate::println!("httpd: failed to start on port {} (TCP listener unavailable?)", port);
    }
}

fn cmd_httpd_stop() {
    if !crate::net::http::is_running() {
        crate::println!("httpd: not running");
        return;
    }
    let port = crate::net::http::server_port().unwrap_or(0);
    crate::net::http::stop();
    crate::println!("httpd: stopped (was on port {})", port);
}

fn cmd_httpd_stats() {
    let s = crate::net::http::stats();
    crate::println!("HTTP server stats:");
    if crate::net::http::is_running() {
        let p = crate::net::http::server_port().unwrap_or(0);
        crate::println!("  status:       running on port {}", p);
    } else {
        crate::println!("  status:       stopped");
    }
    crate::println!("  requests:     {}", s.requests);
    crate::println!("  bytes out:    {}", s.bytes_out);
    crate::println!("  404s:         {}", s.not_found);
    crate::println!("  errors:       {}", s.errors);
}
