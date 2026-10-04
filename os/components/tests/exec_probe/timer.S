/* No ecall during a long finite loop: another task must still get CPU time. */
.section .text
.global _start
_start:
    la t0, saved
    fld fs0, 0(t0)
    li s0, 12345
    li t0, 200000000
1:
    addi t0, t0, -1
    bnez t0, 1b
    li t1, 12345
    bne s0, t1, failed
    la t1, saved
    fld ft0, 0(t1)
    feq.d t1, fs0, ft0
    beqz t1, failed
    li a0, 0
    li a7, 93
    ecall
    unimp
failed:
    li a0, 14
    li a7, 93
    ecall
.section .rodata
.balign 8
saved: .double 41.5
.section .note.GNU-stack,"",@progbits
