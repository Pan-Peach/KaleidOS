OUTPUT_ARCH(riscv)
ENTRY(_start)

PHDRS {
    text PT_LOAD FILEHDR PHDRS FLAGS(5); /* R-X */
    data PT_LOAD FLAGS(6);              /* RW- */
}

SECTIONS {
    . = 0x10000 + SIZEOF_HEADERS;
    .text : { *(.text*) *(.rodata*) } :text
    . = ALIGN(4096);
    .data : { BYTE(0); *(.data*) } :data
    .bss (NOLOAD) : { *(.bss*) *(COMMON) } :data
    /DISCARD/ : { *(.comment) *(.riscv.attributes) *(.note*) *(.eh_frame*) }
}
