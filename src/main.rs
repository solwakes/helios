#![no_std]
#![no_main]

extern crate alloc;

mod alloc_impl;
mod arch;
mod framebuffer;
mod fwcfg;
mod graph;
mod mm;
mod panic;
mod ramfb;
mod shell;
mod task;
mod trap;
mod uart;
#[allow(dead_code)]
mod virtio;

use arch::riscv64 as arch_impl;

/// Kernel entry point — called from boot.S after stack setup.
/// a0 = hart ID, a1 = pointer to device tree blob
#[no_mangle]
pub extern "C" fn kmain(hart_id: usize, _dtb: usize) -> ! {
    // Initialize the heap allocator before anything that might allocate
    alloc_impl::alloc_init();

    // Initialize UART first for early debug output
    uart::init();

    println!();
    println!("========================================");
    println!("  _   _      _ _");
    println!(" | | | | ___| (_) ___  ___");
    println!(" | |_| |/ _ \\ | |/ _ \\/ __|");
    println!(" |  _  |  __/ | | (_) \\__ \\");
    println!(" |_| |_|\\___|_|_|\\___/|___/");
    println!();
    println!("  Everything is a memory.");
    println!("========================================");
    println!();
    println!("[boot] Hart {} reporting for duty", hart_id);
    println!("[boot] Helios v{}", env!("CARGO_PKG_VERSION"));

    // Set up Sv39 identity-mapped page tables and enable paging
    mm::init();

    // Initialize the graph store
    graph::init();

    // Initialize block device and try to load saved graph
    if virtio::blk::init() {
        // Try to load saved graph from disk
        if let Some(blk) = virtio::blk::get_mut() {
            let mut header_sector = [0u8; 512];
            if blk.read_sector(0, &mut header_sector) {
                let data_len = u64::from_le_bytes([
                    header_sector[0], header_sector[1], header_sector[2], header_sector[3],
                    header_sector[4], header_sector[5], header_sector[6], header_sector[7],
                ]) as usize;
                if data_len > 0 && data_len < 16 * 1024 * 1024 {
                    let total_len = 8 + data_len;
                    let sectors = (total_len + 511) / 512;
                    let mut payload = alloc::vec![0u8; sectors * 512];
                    if blk.read(0, &mut payload) {
                        let data = &payload[8..8 + data_len];
                        if let Some(loaded_graph) = graph::persist::deserialize(data) {
                            let nodes = loaded_graph.node_count();
                            let edges = loaded_graph.edge_count();
                            graph::replace(loaded_graph);
                            println!(
                                "[graph] Loaded saved graph from disk: {} nodes, {} edges",
                                nodes, edges
                            );
                        }
                    }
                }
            }
        }
    }

    // Initialize framebuffer (ramfb via fw_cfg)
    framebuffer::init();

    // Set up trap handling and timer interrupts
    trap::init();

    println!();
    println!("[boot] Helios kernel initialized successfully.");
    println!();

    // Initialize cooperative multitasking (creates task #0 = shell)
    task::init();

    // Start the interactive shell
    shell::init();

    // Idle loop: wfi wakes on timer interrupt, then poll UART
    loop {
        unsafe { core::arch::asm!("wfi") };

        // Drain any UART input and feed to the shell
        while let Some(byte) = uart::getc() {
            shell::process_byte(byte);
        }

        // Yield to let other tasks run
        task::yield_now();
    }
}
