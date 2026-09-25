OUTPUT_ARCH(riscv)
ENTRY(_start)

/* RV32 profile: physical identity view, no high-half assumption.
 *
 * Sections are grouped by permission so vm32::runtime can map each group with
 * its real flags (text RX / rodata R / data RW / bss RW).  `.bss.stack` stays
 * OUTSIDE `__bss_load_start..__bss_load_end` so entry32.S never zeroes the
 * stack it is running on.
 */
PHYS_BASE = 0x80200000;

SECTIONS
{
    . = PHYS_BASE;
    __image_load_start = .;

    /* ---- executable (RX) ---- */
    . = ALIGN(4k);
    __text_vma_start = .;
    .text.entry :
    {
        KEEP(*(.text.entry))
    }
    . = ALIGN(4k);
    .text :
    {
        *(.text .text.*)
    }
    __text_vma_end = .;

    /* ---- read-only (R), including the embedded component archive ---- */
    . = ALIGN(4k);
    __rodata_vma_start = .;
    .rodata.entry :
    {
        KEEP(*(.rodata.entry))
    }
    . = ALIGN(4k);
    .rodata :
    {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
    }
    .initpkg :
    {
        __initpkg_start = .;
        *(.initpkg)
        __initpkg_end = .;
    }
    __rodata_vma_end = .;

    /* ---- writable data (RW) ---- */
    . = ALIGN(4k);
    __data_vma_start = .;
    .data :
    {
        *(.data .data.*)
        *(.sdata .sdata.*)
    }
    __data_vma_end = .;

    /* ---- bss (RW, NOLOAD): boot stack first, then the zeroed formal BSS ---- */
    . = ALIGN(4k);
    __bss_vma_start = .;
    .bss.stack (NOLOAD) :
    {
        KEEP(*(.bss.stack))
    }
    . = ALIGN(4k);
    __bss_load_start = .;
    .bss (NOLOAD) :
    {
        *(.bss .bss.*)
        *(.sbss .sbss.*)
    }
    __bss_load_end = .;
    __bss_vma_end = .;

    __image_end = .;

    /DISCARD/ : { *(.eh_frame) }
}
