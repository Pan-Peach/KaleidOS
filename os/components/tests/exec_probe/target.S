/* New argv/data/BSS/FP state after exec. */
.section .text
.global _start
_start:
    ld t0, 0(sp)
    li t1, 2
    bne t0, t1, failed
    ld t0, 16(sp)
    la t1, fresh
1:
    lbu t2, 0(t0)
    lbu t3, 0(t1)
    bne t2, t3, failed
    beqz t2, 2f
    addi t0, t0, 1
    addi t1, t1, 1
    j 1b
2:
    la t0, counter
    ld t1, 0(t0)
    li t2, 55
    bne t1, t2, failed
    la t0, zero_area
    ld t1, 0(t0)
    bnez t1, failed
    fmv.x.d t0, fs0
    bnez t0, failed
    li a0, 7
    j exit
failed:
    li a0, 12
exit:
    li a7, 93
    ecall
    unimp
.section .rodata
fresh: .asciz "fresh"
.section .data
.balign 8
counter: .quad 55
.section .bss
.balign 8
zero_area: .skip 8
.section .note.GNU-stack,"",@progbits
