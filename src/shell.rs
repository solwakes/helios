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

    let mut parts = line.splitn(3, ' ');
    let cmd = parts.next().unwrap_or("");
    let arg1 = parts.next().unwrap_or("");
    let arg2 = parts.next().unwrap_or("");

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
        _ => {
            crate::println!("Unknown command: {}", cmd);
            crate::println!("Type 'help' for available commands.");
        }
    }
}

fn cmd_help() {
    crate::println!("Available commands:");
    crate::println!("  help    - show this help");
    crate::println!("  info    - system information");
    crate::println!("  mem     - hex dump: mem <addr> [count]");
    crate::println!("  poke    - write u32: poke <addr> <value>");
    crate::println!("  timer   - show tick count & uptime");
    crate::println!("  panic   - trigger test panic");
    crate::println!("  fault   - trigger page fault");
    crate::println!("  clear   - clear screen");
    crate::println!("  reboot  - reboot via SBI");
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
