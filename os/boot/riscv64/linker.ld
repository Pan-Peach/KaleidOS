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

    /* 内嵌组件归档（init.kpkg = cpio），由 Makefile 的 rust-objcopy 生成 initpkg.o 提供 */
    .initpkg : ALIGN(4k)
    {
        __initpkg_start = .;
        *(.initpkg)
        __initpkg_end = .;
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