/// Live system node refresh — updates graph nodes with current system state.

use alloc::format;

use crate::arch::riscv64 as arch;
use crate::trap;
use crate::alloc_impl;

/// Timer frequency on QEMU virt (10 MHz).
const TIMER_FREQ: usize = 10_000_000;

/// Well-known node IDs for system nodes.
pub const NODE_SYSTEM: u64 = 2;
pub const NODE_UART0: u64 = 4;
pub const NODE_FB0: u64 = 5;
pub const NODE_MEMORY: u64 = 6;
pub const NODE_TIMER: u64 = 7;
pub const NODE_CPU: u64 = 8;

/// Refresh all system nodes with current live data.
pub fn refresh_system_nodes() {
    refresh_system_info();
    refresh_uart_info();
    refresh_fb_info();
    refresh_memory_info();
    refresh_timer_info();
    refresh_cpu_info();
}

fn uptime() -> (usize, usize) {
    let time = arch::read_time();
    let secs = time / TIMER_FREQ;
    let frac = (time % TIMER_FREQ) / (TIMER_FREQ / 10);
    (secs, frac)
}

fn refresh_system_info() {
    let g = crate::graph::get_mut();
    if let Some(node) = g.get_node_mut(NODE_SYSTEM) {
        let (s, f) = uptime();
        let info = format!(
            "Helios v{}\nArchitecture: RISC-V 64-bit (rv64gc)\nMode: Supervisor\nUptime: {}.{}s",
            env!("CARGO_PKG_VERSION"), s, f
        );
        node.content = info.into_bytes();
    }
}

fn refresh_uart_info() {
    let g = crate::graph::get_mut();
    if let Some(node) = g.get_node_mut(NODE_UART0) {
        let info = format!(
            "NS16550A UART\nBase: 0x10000000\nBaud: 115200\nStatus: active"
        );
        node.content = info.into_bytes();
    }
}

fn refresh_fb_info() {
    let g = crate::graph::get_mut();
    if let Some(node) = g.get_node_mut(NODE_FB0) {
        let info = if let Some(fb) = crate::framebuffer::get() {
            format!(
                "ramfb display\nResolution: {}x{}\nFormat: XRGB8888\nStride: {}\nStatus: active",
                fb.width, fb.height, fb.stride
            )
        } else {
            format!("ramfb display\nStatus: inactive")
        };
        node.content = info.into_bytes();
    }
}

fn refresh_memory_info() {
    let g = crate::graph::get_mut();
    if let Some(node) = g.get_node_mut(NODE_MEMORY) {
        let used = alloc_impl::heap_used();
        let total = alloc_impl::heap_total();
        let satp = arch::read_satp();
        let info = format!(
            "Heap: {:#x} - {:#x} ({} KiB)\nUsed: ~{} KiB\nPage tables: Sv39\nSATP: {:#018x}",
            alloc_impl::heap_start_addr(),
            alloc_impl::heap_end_addr(),
            total / 1024,
            used / 1024,
            satp
        );
        node.content = info.into_bytes();
    }
}

fn refresh_timer_info() {
    let g = crate::graph::get_mut();
    if let Some(node) = g.get_node_mut(NODE_TIMER) {
        let ticks = trap::tick_count();
        let (s, f) = uptime();
        let info = format!(
            "Frequency: 10 MHz\nTicks: {}\nUptime: {}.{}s\nInterval: 100ms",
            ticks, s, f
        );
        node.content = info.into_bytes();
    }
}

fn refresh_cpu_info() {
    let g = crate::graph::get_mut();
    if let Some(node) = g.get_node_mut(NODE_CPU) {
        let sstatus = arch::read_sstatus();
        let info = format!(
            "Hart: 0\nISA: rv64gc\nSATP mode: Sv39\nStatus register: {:#018x}",
            sstatus
        );
        node.content = info.into_bytes();
    }
}
