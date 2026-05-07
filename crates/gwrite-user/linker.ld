/* Linker script for Rust user-mode programs on Helios.
 *
 * The kernel's per-task user VA window spans 0x4000_0000..0x4020_0000
 * (see USER_CODE_BASE / USER_STACK_TOP in src/user.rs):
 *
 *   0x4000_0000 .. 0x4010_0000   exec-edge content (R+W+X, up to 64 pages)
 *   0x4010_0000 .. 0x401F_F000   read/write-edge content
 *   0x401F_F000 .. 0x4020_0000   user stack (R+W)
 *
 * We lay out the Rust binary as a single contiguous image at
 * 0x4000_0000 (one exec edge covers it all). `_start` is tagged
 * `.text.entry` so it lands first in .text and ends up at exactly
 * 0x4000_0000 — the kernel jumps to the first byte of the first exec
 * edge.
 *
 * .bss is placed but since objcopy -O binary drops NOBITS sections we
 * force runtime-writable scratch (the helios-std bump heap) into .data
 * via `#[link_section = ".data.helios_heap"]`. Anything that genuinely
 * lands in .bss will fault at first access; see the helios-std `heap`
 * module for the workaround. */

OUTPUT_ARCH("riscv")
ENTRY(_start)

SECTIONS
{
    . = 0x40000000;

    .text : {
        KEEP(*(.text.entry))
        *(.text .text.*)
    }

    .rodata : ALIGN(16) {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
    }

    /* Fold .bss into .data (both PROGBITS). The raw binary we emit via
     * `objcopy -O binary` is copied byte-for-byte into user pages by
     * the kernel; a NOLOAD .bss would not appear in the binary, so
     * any zero-init `static` (e.g. `AtomicUsize::new(0)`) would be
     * unmapped at runtime. Forcing .bss into .data means those bytes
     * are present in the image (filled with zero by the linker), so
     * the kernel faithfully copies them into writable pages. Costs a
     * small amount of disk size per zero-init global; fine for M31. */
    .data : ALIGN(16) {
        *(.data .data.*)
        *(.sdata .sdata.*)
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        *(COMMON)
    }

    /DISCARD/ : {
        *(.eh_frame)
        *(.eh_frame_hdr)
        *(.note .note.*)
        *(.comment)
        *(.riscv.attributes)
    }
}
