.section .text.entry
.globl _start

_start:
    # OpenSBI:
    # a0: hartid
    # a1: dtb addr

    la sp, boot_stack_top

    call bootstrap_main

1: 
    wfi
    j 1b

.section .bss.stack
.align 12

boot_stack:
    .space 4096*4
    
boot_stack_top: