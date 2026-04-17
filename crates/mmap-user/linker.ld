/* Linker script for Rust user-mode programs on Helios.
 *
 * Identical to crates/ls-user/linker.ld and crates/hello-user/linker.ld;
 * see those files for the full rationale. Short version:
 *
 *   - .text.entry is KEEP'd first so `_start` lands at 0x4000_0000
 *     (the kernel's USER_CODE_BASE — first byte of the exec edge).
 *   - .bss is folded into .data because objcopy -O binary drops
 *     NOBITS sections; any zero-init `static` would otherwise be
 *     unmapped at runtime.
 */

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
