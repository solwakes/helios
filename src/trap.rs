/// Trap handling for Helios — supervisor-mode exceptions and interrupts.
///
/// The assembly entry point uses `sscratch` to swap stacks when a trap comes
/// from U-mode: while a task is in U-mode, `sscratch` holds the task's
/// kernel stack pointer; in S-mode, `sscratch` is zero. On trap entry we
/// atomically swap sp with sscratch — if sp is nonzero afterwards we came
/// from U-mode (and the user sp now lives in sscratch); otherwise we came
/// from S-mode and we swap back to leave sscratch = 0.

use core::arch::global_asm;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::arch::riscv64 as arch;

/// Timer interval: ~100ms at 10 MHz QEMU timer frequency.
const TIMER_INTERVAL: u64 = 1_000_000;

/// Print a tick message every N timer ticks.
const TICK_REPORT_INTERVAL: usize = 10;

/// Global tick counter.
static TICK_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Whether to suppress tick printing (set once shell is active).
static SHELL_ACTIVE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Mark the shell as active, suppressing timer tick output.
pub fn set_shell_active() {
    SHELL_ACTIVE.store(true, Ordering::Relaxed);
}

/// Return the current tick count.
pub fn tick_count() -> usize {
    TICK_COUNT.load(Ordering::Relaxed)
}

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

impl TrapFrame {
    /// Convenience accessors for the saved ABI registers.
    #[inline] pub fn a0(&self) -> usize { self.regs[10] }
    #[inline] pub fn a1(&self) -> usize { self.regs[11] }
    #[inline] pub fn a2(&self) -> usize { self.regs[12] }
    #[inline] pub fn a7(&self) -> usize { self.regs[17] }
    #[inline] pub fn set_a0(&mut self, v: usize) { self.regs[10] = v; }
}

// ---------------------------------------------------------------------------
// Assembly trap entry point
// ---------------------------------------------------------------------------

global_asm!(
    r#"
.align 4
.globl _trap_entry
_trap_entry:
    # -------------------------------------------------------------------
    # Stack switch: while in U-mode, sscratch holds the task's kernel sp
    # and sp holds the user sp. While in S-mode, sscratch is 0.
    # -------------------------------------------------------------------
    csrrw sp, sscratch, sp        # atomic swap sp <-> sscratch
    bnez  sp, 100f                 # if new sp nonzero -> came from U-mode
    # S-mode entry: we swapped 0 into sp and our old sp into sscratch.
    # Swap back so sp = old sp, sscratch = 0.
    csrrw sp, sscratch, sp
100:

    # Allocate the TrapFrame: 33 * 8 = 264, round up to 272 for 16B align.
    addi    sp, sp, -272

    # Save x1, x3-x31
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

    # Decide how to save the ORIGINAL sp (x2): check sstatus.SPP.
    # If SPP=0 (came from U-mode), original sp is in sscratch.
    # If SPP=1 (came from S-mode), original sp = current sp + 272.
    csrr    t0, sstatus
    li      t1, 0x100            # SPP mask
    and     t0, t0, t1
    bnez    t0, 200f             # SPP=1 -> from S-mode
    # From U-mode: sscratch holds user sp; save it.
    csrr    t0, sscratch
    sd      t0, 2*8(sp)
    # Reset sscratch to this task's kernel sp top for the NEXT trap.
    addi    t0, sp, 272
    csrw    sscratch, t0
    j       300f
200:
    # From S-mode: original sp = sp + 272.
    addi    t0, sp, 272
    sd      t0, 2*8(sp)
300:

    # Save sepc.
    csrr    t0, sepc
    sd      t0, 32*8(sp)

    # Call Rust handler with a0 = pointer to TrapFrame.
    mv      a0, sp
    call    trap_handler

    # Restore sepc.
    ld      t0, 32*8(sp)
    csrw    sepc, t0

    # Restore x1, x3-x31.
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

    # Before restoring sp, decide direction based on sstatus.SPP
    # (which trap_handler may have modified — especially when we
    # want to return to S-mode after a U-mode exit).
    csrr    t0, sstatus
    li      t1, 0x100
    and     t0, t0, t1
    bnez    t0, 400f             # SPP=1 -> return to S-mode
    # Returning to U-mode: restore user sp, keep sscratch = kernel sp.
    ld      t0, 2*8(sp)          # t0 = user sp (from frame)
    addi    sp, sp, 272           # sp = kernel sp top
    csrw    sscratch, sp          # sscratch = kernel sp for next trap
    mv      sp, t0                # sp = user sp
    sret
400:
    # Returning to S-mode: discard frame, sret.
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

    // Were we in U-mode before the trap? sstatus.SPP reflects this.
    let sstatus_now = arch::read_sstatus();
    let from_umode = (sstatus_now & 0x100) == 0;

    if is_interrupt {
        match cause_code {
            // Supervisor timer interrupt
            5 => handle_timer_interrupt(from_umode),
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
        // Synchronous exception.
        if from_umode {
            // Dispatch user-mode exceptions to the user subsystem.
            // On ecall we advance sepc past the 4-byte ecall insn.
            if cause_code == 8 {
                frame.sepc = sepc.wrapping_add(4);
                crate::user::handle_syscall(frame);
                return;
            }
            // Any other U-mode exception is a capability violation or
            // program fault — kill the task and long-jump back to the
            // kernel context that launched it.
            crate::user::handle_user_fault(frame, cause_code, stval);
            return;
        }

        // Otherwise it's a fatal kernel exception.
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

fn handle_timer_interrupt(from_umode: bool) {
    let count = TICK_COUNT.fetch_add(1, Ordering::Relaxed) + 1;

    if count % TICK_REPORT_INTERVAL == 0 && !SHELL_ACTIVE.load(Ordering::Relaxed) {
        crate::println!("[timer] tick {}", count);
    }

    // Re-arm the timer for the next interval via stimecmp (Sstc extension)
    let next = arch::read_time() as u64 + TIMER_INTERVAL;
    arch::write_stimecmp(next);

    // If a user task was preempted by this timer interrupt, do NOT try
    // to switch kernel tasks — the kernel task context for a user task
    // is the inline caller on the kernel stack we're using right now,
    // and we'd corrupt it. Just return to U-mode.
    if from_umode {
        return;
    }

    // Preemptive multitasking among kernel tasks.
    crate::task::preemptive_yield();
}

// ---------------------------------------------------------------------------
// Trap subsystem initialisation
// ---------------------------------------------------------------------------

extern "C" {
    fn _trap_entry();
}

/// Bit 1 of `sstatus` — Supervisor Interrupt Enable.
const SSTATUS_SIE: usize = 1 << 1;

/// Bit 18 of `sstatus` — Permit Supervisor access to User memory.
const SSTATUS_SUM: usize = 1 << 18;

/// Bit 5 of `sie` — Supervisor Timer Interrupt Enable.
const SIE_STIE: usize = 1 << 5;

/// Initialise the trap subsystem:
///  1. Install the trap vector (`stvec`).
///  2. Zero sscratch (S-mode sentinel — set nonzero only when in U-mode).
///  3. Enable SUM so the kernel can read/write user pages for syscalls.
///  4. Enable supervisor timer interrupts (`sie.STIE`).
///  5. Arm the first timer via `stimecmp` (Sstc extension).
///  6. Enable global supervisor interrupts (`sstatus.SIE`).
pub fn init() {
    // 1. Set stvec to our trap entry (Direct mode — low bits = 0)
    let trap_entry_addr = _trap_entry as *const () as usize;
    arch::write_stvec(trap_entry_addr);
    crate::println!("[trap] Trap handler installed at {:#x}", trap_entry_addr);

    // 2. sscratch starts at 0 — no user task running yet.
    arch::write_sscratch(0);

    // 3. Enable SUM so syscall handlers can copy to/from user memory.
    let sstatus = arch::read_sstatus();
    arch::write_sstatus(sstatus | SSTATUS_SUM);

    // 4. Enable timer interrupts in sie
    let sie = arch::read_sie();
    arch::write_sie(sie | SIE_STIE);

    // 5. Arm the first timer
    let now = arch::read_time() as u64;
    arch::write_stimecmp(now + TIMER_INTERVAL);

    // 6. Enable global supervisor interrupts in sstatus
    let sstatus = arch::read_sstatus();
    arch::write_sstatus(sstatus | SSTATUS_SIE);

    crate::println!("[trap] Timer interrupts enabled (interval: 100ms), SUM enabled");
}
