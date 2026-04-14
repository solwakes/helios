use core::panic::PanicInfo;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    crate::println!("\n!!! KERNEL PANIC !!!");
    if let Some(location) = info.location() {
        crate::println!(
            "  at {}:{}:{}",
            location.file(),
            location.line(),
            location.column()
        );
    }
    if let Some(message) = info.message().as_str() {
        crate::println!("  {}", message);
    } else {
        crate::println!("  {}", info.message());
    }
    loop {
        unsafe { core::arch::asm!("wfi") };
    }
}
