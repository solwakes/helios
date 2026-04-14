use core::arch::global_asm;

global_asm!(include_str!("boot.S"));

/// Perform a supervisor ecall to OpenSBI.
/// a7 = extension ID (EID), a6 = function ID (FID)
/// a0..a5 = arguments; returns (error, value) in a0, a1.
#[inline(always)]
pub unsafe fn sbi_call(eid: usize, fid: usize, a0: usize, a1: usize, a2: usize) -> (usize, usize) {
    let error: usize;
    let value: usize;
    core::arch::asm!(
        "ecall",
        inlateout("a0") a0 => error,
        inlateout("a1") a1 => value,
        in("a2") a2,
        in("a6") fid,
        in("a7") eid,
    );
    (error, value)
}

/// SBI console putchar (legacy extension 0x01)
pub fn sbi_console_putchar(ch: u8) {
    unsafe {
        sbi_call(0x01, 0, ch as usize, 0, 0);
    }
}

// ---------------------------------------------------------------------------
// CSR helpers for virtual memory
// ---------------------------------------------------------------------------

/// Read the `satp` CSR.
#[inline(always)]
pub fn read_satp() -> usize {
    let val: usize;
    unsafe { core::arch::asm!("csrr {}, satp", out(reg) val) };
    val
}

/// Write the `satp` CSR.
#[inline(always)]
pub fn write_satp(val: usize) {
    unsafe { core::arch::asm!("csrw satp, {}", in(reg) val) };
}

/// Full TLB flush (`sfence.vma zero, zero`).
#[inline(always)]
pub fn sfence_vma() {
    unsafe { core::arch::asm!("sfence.vma zero, zero") };
}

/// Shutdown via SBI SRST extension
pub fn sbi_shutdown() -> ! {
    unsafe {
        sbi_call(0x53525354, 0, 0, 0, 0);
    }
    loop {
        unsafe { core::arch::asm!("wfi") };
    }
}
