.section .text.entry
.globl _start

_start:
    # OpenSBI supplies a0=hartid and a1=DTB.  Keep the image identity-mapped,
    # but enter Sv32 before Rust starts so the RV32 path exercises the same
    # translation boundary as RV64.  One level-1 leaf covers each 4 MiB
    # window, which gives a simple 4 GiB identity map without an early heap.
    auipc s1, 0
    csrw satp, zero
    sfence.vma

    la sp, boot_stack_top

    # Clear the formal image BSS before entering Rust.
    la t0, __bss_load_start
    la t1, __bss_load_end
    bgeu t0, t1, .Lbss_done
.Lbss:
    sw zero, 0(t0)
    addi t0, t0, 4
    bltu t0, t1, .Lbss
.Lbss_done:
    la t0, early_sv32_root
    li t1, 0
    li t2, 1024
.Lsv32_root:
    slli t3, t1, 22
    srli t3, t3, 2
    ori t3, t3, 0xcf             # V|R|W|X|A|D, 4 MiB leaf
    sw t3, 0(t0)
    addi t0, t0, 4
    addi t1, t1, 1
    bltu t1, t2, .Lsv32_root

    la t0, early_sv32_root
    srli t0, t0, 12
    li t1, 0x80000000             # Sv32 MODE=1
    or t0, t0, t1
    sfence.vma
    csrw satp, t0
    sfence.vma

    mv a2, s1
    tail bootstrap_main

.Lhalt:
    wfi
    j .Lhalt

.section .bss.stack
.align 12
boot_stack:
    .space 65536
boot_stack_top:

.section .bss.early_root
.align 12
early_sv32_root:
    .space 4096
