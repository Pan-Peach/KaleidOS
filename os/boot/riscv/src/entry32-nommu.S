.section .text.entry
.globl _start

_start:
    # OpenSBI supplies a0=hartid and a1=DTB.  Keep the flat RV32 image in its
    # link-time identity view; no page table or satp mode is installed here.
    auipc s1, 0

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
    mv a2, s1
    tail bootstrap_main

.Lhalt:
    wfi
    j .Lhalt

.section .bss.stack
.align 12
boot_stack:
    .space 4096*4
boot_stack_top:
