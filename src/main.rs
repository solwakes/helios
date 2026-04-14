#![no_std]
#![no_main]

mod arch;
mod framebuffer;
mod panic;
mod uart;

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

    // Initialize framebuffer (deferred to M2)
    framebuffer::init();

    println!();
    println!("[boot] Helios kernel initialized successfully.");
    println!("[boot] Entering idle loop. Use Ctrl-A X to exit QEMU.");

    // Idle loop
    loop {
        unsafe { core::arch::asm!("wfi") };
    }
}
