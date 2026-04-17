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
        "clear" => cmd_clear(),
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
    crate::println!("                  producer, consumer, pingpong)");
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

fn cmd_clear() {
    // ANSI escape: clear screen and move cursor home
    crate::print!("\x1b[2J\x1b[H");
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
        crate::println!("Usage: spawn <name>");
        crate::println!("Available: counter, fibonacci, busyloop, producer, consumer, pingpong");
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
            crate::println!("Unknown task '{}'. Available: counter, fibonacci, busyloop, producer, consumer, pingpong", name);
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
