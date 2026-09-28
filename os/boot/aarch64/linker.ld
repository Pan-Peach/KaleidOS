/* aarch64 boot skeleton linker script.
 *
 * SKELETON: a minimal layout so the crate can cross-build.  Replace with the
 * real kernel image layout (load address, section alignment, BSS boundaries)
 * when bringing aarch64 up.  See os/boot/riscv/linker.ld for the production
 * shape. */

ENTRY(_start)

SECTIONS
{
    . = 0x40000000;

    .text : {
        __text_start = .;
        *(.text .text.*)
        __text_end = .;
    }

    .rodata : ALIGN(4096) {
        __rodata_start = .;
        *(.rodata .rodata.*)
        __rodata_end = .;
    }

    .data : ALIGN(4096) {
        __data_start = .;
        *(.data .data.*)
        __data_end = .;
    }

    .bss : ALIGN(4096) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        __bss_end = .;
    }

    __kernel_end = .;
}
