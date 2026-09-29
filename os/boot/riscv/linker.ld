OUTPUT_ARCH(riscv)
ENTRY(_start)

/*
 * Current QEMU virt/OpenSBI ELF-loader contract.  This is the preferred
 * physical image address for this boot path, not Core's memory model and not
 * a per-machine build knob.  A future relocatable Image path will keep the
 * link-time offset but derive the actual physical address at _start.
 */
PHYS_BASE = 0x80200000;
HIGH_OFFSET = 0xffffffc000000000;
/* Fixed virtual link address; it is deliberately independent of PHYS_BASE. */
KERNEL_VMA = 0xffffffc080200000;

SECTIONS
{
    /* Low physical/VMA bootstrap: OpenSBI enters here before MMU. */
    . = PHYS_BASE;
    __image_load_start = .;

    . = ALIGN(4k);
    .text.entry :
    {
        KEEP(*(.text.entry))
    }
    __text_entry_vma_start = ADDR(.text.entry);
    __text_entry_vma_end = ADDR(.text.entry) + SIZEOF(.text.entry);
    __text_entry_lma_start = LOADADDR(.text.entry);
    __text_entry_lma_end = LOADADDR(.text.entry) + SIZEOF(.text.entry);

    /*
     * AP 入口 trampoline（secondary64.S）放在正式 `.text`（高 VMA、低 LMA）：
     * AP 从它的 LMA（= 物理地址）执行；高半区代码引用它时是高→高 PC 相对，
     * 不会 PCREL 超范围。它**不能**进 .text.entry——那会与 _start 争抢镜像基址。
     */

    . = ALIGN(8);
    .rodata.entry :
    {
        KEEP(*(.rodata.entry))
    }
    __rodata_entry_vma_start = ADDR(.rodata.entry);
    __rodata_entry_vma_end = ADDR(.rodata.entry) + SIZEOF(.rodata.entry);
    __rodata_entry_lma_start = LOADADDR(.rodata.entry);
    __rodata_entry_lma_end = LOADADDR(.rodata.entry) + SIZEOF(.rodata.entry);

    . = ALIGN(4k);
    .bss.stack (NOLOAD) :
    {
        KEEP(*(.bss.stack))
    }

    . = ALIGN(4k);
    .bss.early_root (NOLOAD) :
    {
        KEEP(*(.bss.early_root))
    }

    __low_bootstrap_end = .;

    /* Formal kernel sections: high VMA, low physical LMA. */
    . = KERNEL_VMA + ALIGN(__low_bootstrap_end - PHYS_BASE, 4k);

    .text : AT(PHYS_BASE + (ADDR(.text) - KERNEL_VMA))
    {
        *(.text .text.*)
    }
    __text_vma_start = ADDR(.text);
    __text_vma_end = ADDR(.text) + SIZEOF(.text);
    __text_lma_start = LOADADDR(.text);
    __text_lma_end = LOADADDR(.text) + SIZEOF(.text);

    . = ALIGN(4k);
    .rodata : AT(PHYS_BASE + (ADDR(.rodata) - KERNEL_VMA))
    {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
    }
    __rodata_vma_start = ADDR(.rodata);
    __rodata_vma_end = ADDR(.rodata) + SIZEOF(.rodata);
    __rodata_lma_start = LOADADDR(.rodata);
    __rodata_lma_end = LOADADDR(.rodata) + SIZEOF(.rodata);

    /* Embedded init.kpkg follows the same high-VMA / low-LMA rule. */
    . = ALIGN(4k);
    .initpkg : AT(PHYS_BASE + (ADDR(.initpkg) - KERNEL_VMA))
    {
        __initpkg_start = .;
        *(.initpkg)
        __initpkg_end = .;
    }
    __initpkg_lma_start = LOADADDR(.initpkg);
    __initpkg_lma_end = LOADADDR(.initpkg) + SIZEOF(.initpkg);

    . = ALIGN(4k);
    .data : AT(PHYS_BASE + (ADDR(.data) - KERNEL_VMA))
    {
        *(.data .data.*)
        *(.sdata .sdata.*)
    }
    __data_vma_start = ADDR(.data);
    __data_vma_end = ADDR(.data) + SIZEOF(.data);
    __data_lma_start = LOADADDR(.data);
    __data_lma_end = LOADADDR(.data) + SIZEOF(.data);

    . = ALIGN(4k);
    .bss (NOLOAD) : AT(PHYS_BASE + (ADDR(.bss) - KERNEL_VMA))
    {
        __bss_start = .;
        __bss_load_start = LOADADDR(.bss);

        *(.bss .bss.*)
        *(.sbss .sbss.*)

        __bss_end = .;
        __bss_load_end = LOADADDR(.bss) + SIZEOF(.bss);
    }
    __bss_vma_start = ADDR(.bss);
    __bss_vma_end = ADDR(.bss) + SIZEOF(.bss);

    /* These are VMA symbols; bootstrap converts them back to PA. */
    __bootstrap_start = KERNEL_VMA;
    __bootstrap_end = .;
    __bootstrap_load_start = PHYS_BASE;
    __bootstrap_load_end = LOADADDR(.bss) + SIZEOF(.bss);

    /DISCARD/ : { *(.eh_frame) }
}
