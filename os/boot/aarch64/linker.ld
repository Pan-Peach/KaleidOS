/*
 * aarch64 boot image layout (QEMU `virt`, `-kernel <elf>`).
 *
 * Contract: QEMU loads the ELF PT_LOAD segments at their link addresses and
 * enters at `_start` with x0 = DTB physical address.  The MMU is OFF, so
 * VMA == LMA == PA; there is no high-half alias and no per-section AT() rule.
 *
 * RAM on QEMU `virt` starts at 0x40000000; the image is linked there so QEMU's
 * ELF loader can place it directly.
 */

OUTPUT_ARCH(aarch64)
ENTRY(_start)

SECTIONS
{
    . = 0x40000000;
    __kernel_start = .;

    /* Entry must be the first thing loaded: `_start` is the ELF entry point. */
    .text : ALIGN(4096)
    {
        KEEP(*(.text.entry))
        *(.text .text.*)
    }

    .rodata : ALIGN(4096)
    {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
    }

    .data : ALIGN(4096)
    {
        *(.data .data.*)
        *(.sdata .sdata.*)
        /* Static (non-PIC) builds should not emit a GOT, but keep a home for
         * it so an unexpected one lands inside the loaded image. */
        *(.got .got.*)
    }

    /*
     * Boot stack: its own NOLOAD region so the entry can zero `.bss` without
     * touching the stack it is already running on.  `__boot_stack_top` is the
     * initial SP (16-byte aligned per AAPCS64).
     *
     * 64 KiB, not 16 KiB: `resource::init` constructs the 8 KiB `IrqTable`
     * (`[Option<IrqRoute>; 256]`) as a stack temporary while the call chain is
     * already ~10 KiB deep.  On a 16 KiB stack that overflowed *below*
     * `__boot_stack_bottom` into `.data` (the global heap / committed machine
     * info are the last objects there), silently corrupting live statics.
     */
    .bss.stack (NOLOAD) : ALIGN(16)
    {
        __boot_stack_bottom = .;
        . += 64 * 1024;
        __boot_stack_top = .;
    }

    .bss (NOLOAD) : ALIGN(16)
    {
        __bss_start = .;
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        *(COMMON)
        __bss_end = .;
    }

    /* The Core reserves [__kernel_start, __kernel_end) as permanent image. */
    __kernel_end = .;

    /DISCARD/ : { *(.eh_frame) *(.eh_frame_hdr) }
}
