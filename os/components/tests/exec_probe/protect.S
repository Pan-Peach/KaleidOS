.section .text.start
.global _start
_start:
    la s0, value
    li t0, 41
    sd t0, 0(s0)
    li a0, -1
    li a1, 4096
    li a2, 1
    li a7, 226
    ecall
    li t0, -22
    bne a0, t0, fail
    mv a0, s0
    li a1, 4096
    li a2, 1
    li a7, 226
    ecall
    bnez a0, fail
    ld t0, 0(s0)
    li t1, 41
    bne t0, t1, fail
    sd t1, 0(s0) # Now read-only: must fault in this process.
fail:
    li a0, 13
    li a7, 93
    ecall
.section .bss
.balign 4096
value: .skip 4096
