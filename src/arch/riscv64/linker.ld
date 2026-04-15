OUTPUT_ARCH("riscv")
ENTRY(_start)

BASE_ADDRESS = 0x80200000;

SECTIONS
{
    . = BASE_ADDRESS;

    .text : {
        PROVIDE(_stext = .);
        *(.text.entry)
        *(.text .text.*)
        PROVIDE(_etext = .);
    }

    . = ALIGN(4096);
    .rodata : {
        PROVIDE(_srodata = .);
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
        PROVIDE(_erodata = .);
    }

    . = ALIGN(4096);
    .data : {
        PROVIDE(_sdata = .);
        *(.data .data.*)
        *(.sdata .sdata.*)
        PROVIDE(_edata = .);
    }

    . = ALIGN(4096);
    .bss : {
        PROVIDE(_sbss = .);
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        PROVIDE(_ebss = .);
    }

    PROVIDE(_end = .);

    /* Stack: 128 KiB */
    . = ALIGN(16);
    PROVIDE(_stack_start = .);
    . += 128K;
    PROVIDE(_stack_end = .);

    /* Heap: 32 MiB for dynamic allocation (Doom needs ~16MB+) */
    . = ALIGN(4096);
    PROVIDE(_heap_start = .);
    . += 32M;
    PROVIDE(_heap_end = .);

    /DISCARD/ : {
        *(.eh_frame)
        *(.note .note.*)
    }
}
