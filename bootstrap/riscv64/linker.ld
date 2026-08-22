OUTPUT_ARCH(riscv)
ENTRY(_start)

SECTIONS
{
    . = 0x80200000;

    __bootstrap_start = .;

    .text : ALIGN(4k)
    {
        KEEP(*(.text.entry))
        *(.text .text.*)
    }

    .rodata : ALIGN(4k)
    {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
    }

    .data : ALIGN(4k)
    {
        *(.data .data.*)
        *(.sdata .sdata.*)
    }

    .bss (NOLOAD) : ALIGN(4k)
    {
        *(.bss.stack)   # 预留栈空间，避免 bss 段被清零时覆盖栈

        __bss_start = .;

        *(.bss .bss.*)
        *(.sbss .sbss.*)

        __bss_end = .;
    }

    . = ALIGN(4k);
    __bootstrap_end = .;

    /DISCARD/ : { *(.eh_frame) }
}