/// Trap handling for Helios — supervisor-mode exceptions and interrupts.

use core::arch::global_asm;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::arch::riscv64 as arch;

/// Timer interval: ~100ms at 10 MHz QEMU timer frequency.
const TIMER_INTERVAL: u64 = 1_000_000;

/// Print a tick message every N timer ticks.
const TICK_REPORT_INTERVAL: usize = 10;

/// Global tick counter.
static TICK_COUNT: AtomicUsize = AtomicUsize::new(0);

// ---------------------------------------------------------------------------
// TrapFrame — saved register state
// ---------------------------------------------------------------------------

/// Saved register state pushed onto the stack by the trap entry trampoline.
/// Slots 0..31 correspond to x0..x31; slot 32 holds sepc.
#[repr(C)]
pub struct TrapFrame {
    pub regs: [usize; 32],
    pub sepc: usize,
}

// ---------------------------------------------------------------------------
// Assembly trap entry point
// ---------------------------------------------------------------------------

global_asm!(
    r#"
.align 4
.globl _trap_entry
_trap_entry:
    # Allocate space for TrapFrame: 33 * 8 = 264, round up to 272 for 16-byte alignment
    addi    sp, sp, -272

    # Save general-purpose registers (skip x0=zero, x2=sp saved below)
    sd      x1,  1*8(sp)
    sd      x3,  3*8(sp)
    sd      x4,  4*8(sp)
    sd      x5,  5*8(sp)
    sd      x6,  6*8(sp)
    sd      x7,  7*8(sp)
    sd      x8,  8*8(sp)
    sd      x9,  9*8(sp)
    sd      x10, 10*8(sp)
    sd      x11, 11*8(sp)
    sd      x12, 12*8(sp)
    sd      x13, 13*8(sp)
    sd      x14, 14*8(sp)
    sd      x15, 15*8(sp)
    sd      x16, 16*8(sp)
    sd      x17, 17*8(sp)
    sd      x18, 18*8(sp)
    sd      x19, 19*8(sp)
    sd      x20, 20*8(sp)
    sd      x21, 21*8(sp)
    sd      x22, 22*8(sp)
    sd      x23, 23*8(sp)
    sd      x24, 24*8(sp)
    sd      x25, 25*8(sp)
    sd      x26, 26*8(sp)
    sd      x27, 27*8(sp)
    sd      x28, 28*8(sp)
    sd      x29, 29*8(sp)
    sd      x30, 30*8(sp)
    sd      x31, 31*8(sp)

    # Save original sp (before addi) into the x2 slot
    addi    t0, sp, 272
    sd      t0, 2*8(sp)

    # Save sepc
    csrr    t0, sepc
    sd      t0, 32*8(sp)

    # Call Rust trap handler: a0 = pointer to TrapFrame
    mv      a0, sp
    call    trap_handler

    # Restore sepc (handler may have modified it)
    ld      t0, 32*8(sp)
    csrw    sepc, t0

    # Restore general-purpose registers
    ld      x1,  1*8(sp)
    ld      x3,  3*8(sp)
    ld      x4,  4*8(sp)
    ld      x5,  5*8(sp)
    ld      x6,  6*8(sp)
    ld      x7,  7*8(sp)
    ld      x8,  8*8(sp)
    ld      x9,  9*8(sp)
    ld      x10, 10*8(sp)
    ld      x11, 11*8(sp)
    ld      x12, 12*8(sp)
    ld      x13, 13*8(sp)
    ld      x14, 14*8(sp)
    ld      x15, 15*8(sp)
    ld      x16, 16*8(sp)
    ld      x17, 17*8(sp)
    ld      x18, 18*8(sp)
    ld      x19, 19*8(sp)
    ld      x20, 20*8(sp)
    ld      x21, 21*8(sp)
    ld      x22, 22*8(sp)
    ld      x23, 23*8(sp)
    ld      x24, 24*8(sp)
    ld      x25, 25*8(sp)
    ld      x26, 26*8(sp)
    ld      x27, 27*8(sp)
    ld      x28, 28*8(sp)
    ld      x29, 29*8(sp)
    ld      x30, 30*8(sp)
    ld      x31, 31*8(sp)

    addi    sp, sp, 272
    sret
"#
);

// ---------------------------------------------------------------------------
// Rust trap handler — called from assembly
// ---------------------------------------------------------------------------

/// Rust-level trap handler. Called from `_trap_entry` with a pointer to the
/// saved `TrapFrame` on the stack.
#[no_mangle]
pub extern "C" fn trap_handler(frame: &mut TrapFrame) {
    let scause = arch::read_scause();
    let stval = arch::read_stval();
    let sepc = frame.sepc;

    let is_interrupt = (scause >> 63) & 1 == 1;
    let cause_code = scause & !(1usize << 63);

    if is_interrupt {
        match cause_code {
            // Supervisor timer interrupt
            5 => handle_timer_interrupt(),
            // Supervisor software interrupt
            1 => {
                crate::println!("[trap] Supervisor software interrupt");
            }
            // Supervisor external interrupt
            9 => {
                crate::println!("[trap] Supervisor external interrupt");
            }
            _ => {
                crate::println!(
                    "[trap] Unknown interrupt: cause={}, sepc={:#x}",
                    cause_code,
                    sepc
                );
            }
        }
    } else {
        // Exception — print diagnostic info
        let cause_name = match cause_code {
            0 => "Instruction address misaligned",
            1 => "Instruction access fault",
            2 => "Illegal instruction",
            3 => "Breakpoint",
            4 => "Load address misaligned",
            5 => "Load access fault",
            6 => "Store address misaligned",
            7 => "Store access fault",
            8 => "Environment call from U-mode",
            9 => "Environment call from S-mode",
            12 => "Instruction page fault",
            13 => "Load page fault",
            15 => "Store/AMO page fault",
            _ => "Unknown exception",
        };

        crate::println!();
        crate::println!("!!! TRAP: {} (cause={})", cause_name, cause_code);
        crate::println!("    sepc  = {:#018x}", sepc);
        crate::println!("    stval = {:#018x}", stval);
        crate::println!("    Halting.");

        loop {
            unsafe { core::arch::asm!("wfi") };
        }
    }
}

// ---------------------------------------------------------------------------
// Timer interrupt handler
// ---------------------------------------------------------------------------

fn handle_timer_interrupt() {
    let count = TICK_COUNT.fetch_add(1, Ordering::Relaxed) + 1;

    if count % TICK_REPORT_INTERVAL == 0 {
        crate::println!("[timer] tick {}", count);
    }

    // Re-arm the timer for the next interval via stimecmp (Sstc extension)
    let next = arch::read_time() as u64 + TIMER_INTERVAL;
    arch::write_stimecmp(next);
}

// ---------------------------------------------------------------------------
// Trap subsystem initialisation
// ---------------------------------------------------------------------------

extern "C" {
    fn _trap_entry();
}

/// Bit 1 of `sstatus` — Supervisor Interrupt Enable.
const SSTATUS_SIE: usize = 1 << 1;

/// Bit 5 of `sie` — Supervisor Timer Interrupt Enable.
const SIE_STIE: usize = 1 << 5;

/// Initialise the trap subsystem:
///  1. Install the trap vector (`stvec`).
///  2. Enable supervisor timer interrupts (`sie.STIE`).
///  3. Arm the first timer via `stimecmp` (Sstc extension).
///  4. Enable global supervisor interrupts (`sstatus.SIE`).
pub fn init() {
    // 1. Set stvec to our trap entry (Direct mode — low bits = 0)
    let trap_entry_addr = _trap_entry as *const () as usize;
    arch::write_stvec(trap_entry_addr);
    crate::println!("[trap] Trap handler installed at {:#x}", trap_entry_addr);

    // 2. Enable timer interrupts in sie
    let sie = arch::read_sie();
    arch::write_sie(sie | SIE_STIE);

    // 3. Arm the first timer
    let now = arch::read_time() as u64;
    arch::write_stimecmp(now + TIMER_INTERVAL);

    // 4. Enable global supervisor interrupts in sstatus
    let sstatus = arch::read_sstatus();
    arch::write_sstatus(sstatus | SSTATUS_SIE);

    crate::println!("[trap] Timer interrupts enabled (interval: 100ms)");
}
