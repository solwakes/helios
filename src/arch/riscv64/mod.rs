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

/// Reboot via SBI SRST extension (reset_type=0 warm, reason=0)
pub fn sbi_reboot() -> ! {
    // SRST EID=0x53525354, FID=0, reset_type=1 (cold reboot), reason=0
    unsafe {
        sbi_call(0x53525354, 0, 1, 0, 0);
    }
    loop {
        unsafe { core::arch::asm!("wfi") };
    }
}

// ---------------------------------------------------------------------------
// CSR helpers for trap handling
// ---------------------------------------------------------------------------

/// Read the `scause` CSR.
#[inline(always)]
pub fn read_scause() -> usize {
    let val: usize;
    unsafe { core::arch::asm!("csrr {}, scause", out(reg) val) };
    val
}

/// Read the `stval` CSR.
#[inline(always)]
pub fn read_stval() -> usize {
    let val: usize;
    unsafe { core::arch::asm!("csrr {}, stval", out(reg) val) };
    val
}

/// Read the `sepc` CSR.
#[inline(always)]
pub fn read_sepc() -> usize {
    let val: usize;
    unsafe { core::arch::asm!("csrr {}, sepc", out(reg) val) };
    val
}

/// Write the `sepc` CSR.
#[inline(always)]
pub fn write_sepc(val: usize) {
    unsafe { core::arch::asm!("csrw sepc, {}", in(reg) val) };
}

/// Read the `sstatus` CSR.
#[inline(always)]
pub fn read_sstatus() -> usize {
    let val: usize;
    unsafe { core::arch::asm!("csrr {}, sstatus", out(reg) val) };
    val
}

/// Write the `sstatus` CSR.
#[inline(always)]
pub fn write_sstatus(val: usize) {
    unsafe { core::arch::asm!("csrw sstatus, {}", in(reg) val) };
}

/// Disable supervisor interrupts, returning the previous sstatus value.
#[inline(always)]
pub fn interrupts_disable() -> usize {
    let prev = read_sstatus();
    write_sstatus(prev & !0x2); // clear SIE bit (bit 1)
    prev
}

/// Restore supervisor interrupts to a previous state.
#[inline(always)]
pub fn interrupts_restore(prev_sstatus: usize) {
    if prev_sstatus & 0x2 != 0 {
        // SIE was set before — re-enable
        let cur = read_sstatus();
        write_sstatus(cur | 0x2);
    }
}

/// Read the `sie` CSR.
#[inline(always)]
pub fn read_sie() -> usize {
    let val: usize;
    unsafe { core::arch::asm!("csrr {}, sie", out(reg) val) };
    val
}

/// Write the `sie` CSR.
#[inline(always)]
pub fn write_sie(val: usize) {
    unsafe { core::arch::asm!("csrw sie, {}", in(reg) val) };
}

/// Write the `stvec` CSR.
#[inline(always)]
pub fn write_stvec(val: usize) {
    unsafe { core::arch::asm!("csrw stvec, {}", in(reg) val) };
}

/// Read the `sscratch` CSR.
#[inline(always)]
pub fn read_sscratch() -> usize {
    let val: usize;
    unsafe { core::arch::asm!("csrr {}, sscratch", out(reg) val) };
    val
}

/// Write the `sscratch` CSR.
#[inline(always)]
pub fn write_sscratch(val: usize) {
    unsafe { core::arch::asm!("csrw sscratch, {}", in(reg) val) };
}

/// Flush instruction cache (`fence.i`).
#[inline(always)]
pub fn fence_i() {
    unsafe { core::arch::asm!("fence.i") };
}

/// Read the current time via the `rdtime` pseudo-instruction.
#[inline(always)]
pub fn read_time() -> usize {
    let val: usize;
    unsafe { core::arch::asm!("rdtime {}", out(reg) val) };
    val
}

/// SBI set timer (TIME extension, EID=0x54494D45, FID=0)
/// Returns (error, value) from SBI.
pub fn sbi_set_timer(stime_value: u64) -> (usize, usize) {
    unsafe {
        sbi_call(0x54494D45, 0, stime_value as usize, 0, 0)
    }
}

/// Write the `stimecmp` CSR directly (Sstc extension, CSR 0x14D).
/// This programs the timer comparator for S-mode timer interrupts.
#[inline(always)]
pub fn write_stimecmp(val: u64) {
    unsafe { core::arch::asm!("csrw 0x14D, {}", in(reg) val as usize) };
}
