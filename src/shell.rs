/// Interactive UART shell for Helios.

use crate::arch::riscv64 as arch;
use crate::trap;

const MAX_LINE: usize = 256;
const PROMPT: &str = "helios> ";

/// Timer frequency on QEMU virt (10 MHz).
const TIMER_FREQ: usize = 10_000_000;

static mut LINE_BUF: [u8; MAX_LINE] = [0u8; MAX_LINE];
static mut LINE_LEN: usize = 0;

/// Print the prompt and mark the shell as active.
pub fn init() {
    trap::set_shell_active();
    crate::print!("{}", PROMPT);
}

/// Process a single byte received from the UART.
pub fn process_byte(byte: u8) {
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
                }
            }
            // Printable ASCII
            0x20..=0x7E => {
                if LINE_LEN < MAX_LINE {
                    LINE_BUF[LINE_LEN] = byte;
                    LINE_LEN += 1;
                    crate::uart::putc(byte);
                }
            }
            // Ignore other bytes (e.g. escape sequences)
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
        "status" => cmd_status(),
        "save" => cmd_save(),
        "load" => cmd_load(),
        "disk" => cmd_disk(),
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
    crate::println!("  mknode <t> <n>- create node (text/binary/config/system/dir)");
    crate::println!("  edge <f> <l> <t> - add edge from node f to t");
    crate::println!("  set <id> ...  - set node content");
    crate::println!("  cat <id>      - show node content");
    crate::println!("  walk <id>     - walk node edges");
    crate::println!("  find <name>   - find nodes by name");
    crate::println!("  rm <id>       - remove a node");
    crate::println!("  render        - re-render graph on framebuffer");
    crate::println!("  status        - live system overview");
    crate::println!("Disk commands:");
    crate::println!("  save          - save graph to disk");
    crate::println!("  load          - load graph from disk");
    crate::println!("  disk          - show disk info");
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
            if byte >= 0x20 && byte <= 0x7E {
                crate::uart::putc(byte);
            } else {
                crate::uart::putc(b'.');
            }
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
    if node.content.is_empty() {
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
        crate::println!("Types: text, binary, config, system, dir");
        return;
    }
    let type_tag = match crate::graph::NodeType::from_str(type_str) {
        Some(t) => t,
        None => {
            crate::println!("Unknown type '{}'. Use: text, binary, config, system, dir", type_str);
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
            if node.content.is_empty() {
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
        crate::framebuffer::render_graph();
        crate::println!("Graph rendered to framebuffer.");
    } else {
        crate::println!("No framebuffer available (UART-only mode).");
    }
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
