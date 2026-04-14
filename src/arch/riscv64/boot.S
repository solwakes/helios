.attribute arch, "rv64gc"

.section .text.entry
.globl _start
_start:
    # OpenSBI drops us here in S-mode with a0 = hartid, a1 = dtb pointer
    # Only let hart 0 proceed; park all others
    bnez    a0, .park

    # Clear BSS
    la      t0, _sbss
    la      t1, _ebss
.clear_bss:
    bgeu    t0, t1, .bss_done
    sd      zero, 0(t0)
    addi    t0, t0, 8
    j       .clear_bss
.bss_done:

    # Set up stack (stack grows downward)
    la      sp, _stack_end

    # Save hartid and dtb pointer for Rust
    # a0 = hartid, a1 = dtb are already in the right argument registers
    call    kmain

    # If kmain returns, park
.park:
    wfi
    j       .park
