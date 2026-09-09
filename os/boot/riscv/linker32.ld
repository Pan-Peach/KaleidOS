OUTPUT_ARCH(riscv)
ENTRY(_start)

/* RV32 profile: physical identity view, no high-half assumption. */
PHYS_BASE = 0x80200000;

SECTIONS
{
    . = PHYS_BASE;
    __image_load_start = .;

    . = ALIGN(4k);
    .text.entry :
    {
        KEEP(*(.text.entry))
    }

    . = ALIGN(8);
    .rodata.entry :
    {
        KEEP(*(.rodata.entry))
    }

    . = ALIGN(4k);
    .bss.stack (NOLOAD) :
    {
        KEEP(*(.bss.stack))
    }

    . = ALIGN(4k);
    .text :
    {
        *(.text .text.*)
    }

    . = ALIGN(4k);
    .rodata :
    {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
    }

    . = ALIGN(4k);
    .initpkg :
    {
        __initpkg_start = .;
        *(.initpkg)
        __initpkg_end = .;
    }

    . = ALIGN(4k);
    .data :
    {
        *(.data .data.*)
        *(.sdata .sdata.*)
    }

    . = ALIGN(4k);
    .bss (NOLOAD) :
    {
        __bss_load_start = .;
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        __bss_load_end = .;
    }

    __image_end = .;

    /DISCARD/ : { *(.eh_frame) }
}
