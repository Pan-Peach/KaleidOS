/* x86_64 boot image layout: fixed physical load at 0x100000, ET_EXEC.
 *
 * The boot protocol (PVH; see src/main.rs) enters the linked `_start` at its
 * physical address: there is no relocation, no high-half split and no dynamic
 * loader.  The Multiboot2 header must sit completely inside the first 32 KiB
 * of the image (and 8-byte aligned), so it is placed first; the PVH note is a
 * normal allocated section that the linker turns into a PT_NOTE segment.
 *
 * `.boot.stack` / `.boot.pagetables` are NOLOAD and deliberately outside
 * `__bss_start..__bss_end`: `entry.S` uses the stack and builds the early page
 * tables *before* it zeroes `.bss`, so the clear must not touch them.
 */

OUTPUT_ARCH(i386:x86-64)
ENTRY(_start)

SECTIONS
{
    . = 0x100000;
    __kernel_start = .;

    /* Multiboot2 header: first section, well within the first 32 KiB. */
    .multiboot2 : {
        KEEP(*(.multiboot2))
    }

    . = ALIGN(4096);
    .text.entry : {
        KEEP(*(.text.entry))
    }

    . = ALIGN(4096);
    .text : {
        *(.text .text.*)
    }

    . = ALIGN(4096);
    .rodata : {
        *(.rodata .rodata.*)
    }

    /* Xen PVH PHYS32 entry note -> PT_NOTE segment. */
    .note.Xen : {
        KEEP(*(.note.Xen))
    }

    . = ALIGN(4096);
    .data : {
        *(.data .data.*)
    }

    /* Boot stack: used before `.bss` is cleared. */
    . = ALIGN(4096);
    .boot.stack (NOLOAD) : {
        KEEP(*(.boot.stack))
    }

    /* Early identity page tables: built before `.bss` is cleared. */
    . = ALIGN(4096);
    .boot.pagetables (NOLOAD) : {
        KEEP(*(.boot.pagetables))
    }

    . = ALIGN(4096);
    .bss (NOLOAD) : {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        __bss_end = .;
    }

    /* Half-open image range; bootstrap passes it to Core as `reserved`. */
    __kernel_end = .;

    /DISCARD/ : {
        *(.eh_frame)
        *(.comment)
        *(.note.GNU*)
        *(.note.gnu*)
    }
}
