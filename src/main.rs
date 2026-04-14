#![no_std]
#![no_main]

extern crate alloc;

mod alloc_impl;
mod arch;
mod framebuffer;
mod fwcfg;
mod panic;
mod ramfb;
mod uart;
#[allow(dead_code)]
mod virtio;

use arch::riscv64 as arch_impl;

/// Kernel entry point — called from boot.S after stack setup.
/// a0 = hart ID, a1 = pointer to device tree blob
#[no_mangle]
pub extern "C" fn kmain(hart_id: usize, _dtb: usize) -> ! {
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

    // Test allocator
    {
        extern "C" {
            static _heap_start: u8;
            static _heap_end: u8;
        }
        let hs = unsafe { &_heap_start as *const u8 as usize };
        let he = unsafe { &_heap_end as *const u8 as usize };
        println!("[heap] range: {:#x} - {:#x} ({} KiB)", hs, he, (he - hs) / 1024);

        let layout = core::alloc::Layout::from_size_align(64, 8).unwrap();
        println!("[heap] attempting alloc...");
        let ptr = unsafe { alloc::alloc::alloc(layout) };
        println!("[heap] alloc returned: {:#x}", ptr as usize);
    }

    // Initialize framebuffer (ramfb via fw_cfg)
    framebuffer::init();

    println!();
    println!("[boot] Helios kernel initialized successfully.");
    println!("[boot] Entering idle loop. Use Ctrl-A X to exit QEMU.");

    // Idle loop
    loop {
        unsafe { core::arch::asm!("wfi") };
    }
}
