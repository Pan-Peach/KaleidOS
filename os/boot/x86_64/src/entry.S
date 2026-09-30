/* x86_64 boot entry: 32-bit entry -> long mode -> Rust.
 *
 * The image carries two boot descriptors:
 *
 *   1. A Multiboot2 header (first 32 KiB, 8-byte aligned) per the boot
 *      contract.  The host QEMU (4.2 loader) has no Multiboot2 support, so
 *      this header is inert there; a future QEMU that implements MB2 will
 *      enter `_start` with EAX=0x36d76289 and EBX=MB2 info PA.
 *   2. A Xen PVH `XEN_ELFNOTE_PHYS32_ENTRY` note, which is the path QEMU
 *      actually uses: it loads the ELF64 image and enters `_start` in 32-bit
 *      protected mode with EBX = physical `hvm_start_info`.
 *
 * Both protocols agree on the entry state: 32-bit protected mode, paging off,
 * interrupts off, flat segments.  `_start` therefore:
 *
 *   cli -> own flat GDT -> PAE + 2 MiB identity page tables (long mode needs
 *   paging) -> EFER.LME -> CR0.PG -> ljmp to 64-bit code -> fresh stack ->
 *   clear .bss -> `bootstrap_main(magic, info_pa)`.
 *
 * Identity map: 0..4 GiB via four 2 MiB-page PDs (qemu64 has no 1 GiB pages).
 * This covers the kernel image (1 MiB..), all RAM below 4 GiB, and the legacy
 * MMIO windows.  There is no high-half split in this bring-up.
 */


/* ------------------------------------------------------------------ */
/* Multiboot2 header                                                    */
/* ------------------------------------------------------------------ */
.section .multiboot2,"a"
.align 8
mb2_header:
    .long 0xE85250D6                    /* magic */
    .long 0                             /* architecture: i386 */
    .long mb2_header_end - mb2_header   /* header length */
    .long -(0xE85250D6 + 0 + (mb2_header_end - mb2_header))
    .align 8
    .short 0                            /* end tag */
    .short 0
    .long 8
mb2_header_end:

/* ------------------------------------------------------------------ */
/* Xen PVH note (the descriptor QEMU's `-kernel` actually honors here)  */
/* ------------------------------------------------------------------ */
.section .note.Xen,"a",@note
.align 4
    .long 4                             /* namesz */
    .long 4                             /* descsz: 32-bit entry address */
    .long 18                            /* XEN_ELFNOTE_PHYS32_ENTRY */
    .asciz "Xen"
    .align 4
    .long _start
    .long 0                             /* pad to 8: QEMU reads a size_t */

/* ------------------------------------------------------------------ */
/* 32-bit entry                                                         */
/* ------------------------------------------------------------------ */
.section .text.entry,"ax"
.code32
.globl _start
_start:
    cli
    cld
    /* Preserve the boot descriptors before any register use. */
    movl %eax, boot_magic
    movl %ebx, boot_info

    /* Own flat GDT (the loader's GDT must not be trusted):
     *   0x08 = 32-bit code, 0x10 = 64-bit code, 0x18 = data. */
    lgdt gdt_desc
    ljmp $0x08, $1f
1:
    movw $0x18, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss
    movw %ax, %fs
    movw %ax, %gs

    movl $boot_stack_top, %esp
    andl $-16, %esp

    /* Zero the whole early table set (PML4 + PDPT + 4 PDs; the four PDs are
     * then fully populated, PML4/PDPT get their few entries).  Explicit, so
     * the mapping never relies on loader BSS zeroing of this NOLOAD section. */
    movl $pml4, %edi
    xorl %eax, %eax
    movl $((4096 * 6) / 4), %ecx
    rep stosl

    /* PML4[0] = PDPT | P | RW */
    movl $pdpt, %eax
    orl $3, %eax
    movl %eax, pml4

    /* For each 1 GiB window i in 0..4:
     *   PDPT[i] = (pd_tables + i*4096) | P | RW
     *   PD[i][j] = (i*1GiB + j*2MiB) | P | RW | PS   (j in 0..512) */
    xorl %ecx, %ecx
2:
    movl %ecx, %eax
    shll $12, %eax
    addl $pd_tables, %eax
    orl $3, %eax
    movl %eax, pdpt(,%ecx,8)          /* 8-byte page-table entries */

    movl %ecx, %edi
    shll $12, %edi
    addl $pd_tables, %edi

    movl %ecx, %ebx
    shll $30, %ebx              /* window base = i GiB */

    xorl %eax, %eax
3:
    movl %eax, %edx
    shll $21, %edx              /* j * 2 MiB */
    addl %ebx, %edx
    orl $0x83, %edx             /* P | RW | PS */
    movl %edx, (%edi,%eax,8)          /* 8-byte page-table entries */
    incl %eax
    cmpl $512, %eax
    jne 3b
    incl %ecx
    cmpl $4, %ecx
    jne 2b

    /* CR4: PAE (long mode), OSFXSR + OSXMMEXCPT (SSE for Rust codegen). */
    movl %cr4, %eax
    orl $((1 << 5) | (1 << 9) | (1 << 10)), %eax
    movl %eax, %cr4

    /* CR3 = PML4 physical address. */
    movl $pml4, %eax
    movl %eax, %cr3

    /* EFER.LME = 1. */
    movl $0xC0000080, %ecx
    rdmsr
    orl $0x100, %eax
    wrmsr

    /* CR0: PG | MP, clear EM. */
    movl %cr0, %eax
    orl $((1 << 31) | (1 << 1)), %eax
    andl $(~(1 << 2)), %eax
    movl %eax, %cr0

    /* Far jump through the 64-bit code descriptor; we are now in long mode. */
    ljmp $0x10, $long_mode_entry

/* ------------------------------------------------------------------ */
/* 64-bit entry                                                         */
/* ------------------------------------------------------------------ */
.code64
long_mode_entry:
    movw $0x18, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss
    movw %ax, %fs
    movw %ax, %gs

    leaq boot_stack_top(%rip), %rsp
    andq $-16, %rsp

    /* Zero .bss (the boot stack and page tables live in their own output
     * sections, outside __bss_start..__bss_end). */
    leaq __bss_start(%rip), %rdi
    leaq __bss_end(%rip), %rcx
    subq %rdi, %rcx
    addq $7, %rcx
    shrq $3, %rcx
    xorl %eax, %eax
    rep stosq

    movl boot_magic(%rip), %edi
    movl boot_info(%rip), %esi
    call bootstrap_main

    /* bootstrap_main never returns; park the CPU if it somehow does. */
4:  hlt
    jmp 4b

/* ------------------------------------------------------------------ */
/* Boot data (loaded; deliberately not .bss, which is cleared later)    */
/* ------------------------------------------------------------------ */
.section .data,"aw"
.align 4
boot_magic: .long 0
boot_info:  .long 0

/* ------------------------------------------------------------------ */
/* GDT                                                                  */
/* ------------------------------------------------------------------ */
.section .rodata,"a"
.align 8
gdt:
    .quad 0                             /* null */
    .quad 0x00cf9a000000ffff            /* 0x08: 32-bit code, base 0, limit 4G */
    .quad 0x00af9a000000ffff            /* 0x10: 64-bit code (L=1, D=0) */
    .quad 0x00cf92000000ffff            /* 0x18: data */
gdt_end:
.align 4
gdt_desc:
    .short gdt_end - gdt - 1
    .long gdt

/* ------------------------------------------------------------------ */
/* Boot stack (outside the .bss clear range)                            */
/* ------------------------------------------------------------------ */
.section .boot.stack,"aw",@nobits
.align 16
boot_stack:
    .space 65536
.globl boot_stack_top
boot_stack_top:

/* ------------------------------------------------------------------ */
/* Early page tables (explicitly zeroed/initialized in asm above; they  */
/* must survive the .bss clear, so they live in their own section)      */
/* ------------------------------------------------------------------ */
.section .boot.pagetables,"aw",@nobits
.align 4096
.globl pml4
pml4:
    .space 4096
pdpt:
    .space 4096
pd_tables:
    .space 4096 * 4
