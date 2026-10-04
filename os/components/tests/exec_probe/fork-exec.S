/* Independent parent/child memory + FP registers, failed exec rollback,
 * successful replacement by a different image, and wait/reap semantics. */
.section .text
.global _start
_start:
    li t0, 0x4044c00000000000 /* 41.5 */
    fmv.d.x fs0, t0
    li a0, 17
    li a1, 0
    li a2, 0
    li a3, 0
    li a4, 0
    li a7, 220
    ecall
    bltz a0, failed
    beqz a0, child
    mv s0, a0
    /* A failed status copy must not consume a zombie. */
    mv a0, s0
    li a1, -1
    li a2, 0
    li a3, 0
    li a7, 260
    ecall
    li t0, -14
    bne a0, t0, failed
    mv a0, s0
    la a1, wait_status
    li a2, 0
    li a3, 0
    li a7, 260
    ecall
    bne a0, s0, failed
    la t0, wait_status
    lw t1, 0(t0)
    li t2, 1792 /* exit(7) << 8 */
    bne t1, t2, failed
    la t0, counter
    ld t1, 0(t0)
    li t2, 41
    bne t1, t2, failed
    fmv.x.d t1, fs0
    li t2, 0x4044c00000000000
    bne t1, t2, failed
    mv a0, s0
    li a1, 0
    li a2, 0
    li a3, 0
    li a7, 260
    ecall
    li t0, -10 /* ECHILD: already reaped */
    bne a0, t0, failed
    li a0, 1
    la a1, message
    li a2, 18
    li a7, 64
    ecall
    li a0, 0
    j exit
child:
    la t0, counter
    li t1, 99
    sd t1, 0(t0)
    li t0, 0x4053400000000000 /* 77 */
    fmv.d.x fs0, t0
    la a0, broken
    la a1, arguments
    li a2, 0
    li a7, 221
    ecall
    li t0, -8 /* ENOEXEC, old image still running */
    bne a0, t0, failed
    la t0, counter
    ld t1, 0(t0)
    li t2, 99
    bne t1, t2, failed
    la a0, target
    la a1, arguments
    li a2, 0
    li a7, 221
    ecall
    /* Successful exec must never return into this image. */
failed:
    li a0, 11
exit:
    li a7, 93
    ecall
    unimp
.section .rodata
message: .ascii "EXEC_FORK_EXEC_OK\n" /* 18 bytes */
broken: .asciz "/broken"
target: .asciz "/target"
fresh: .asciz "fresh"
.section .data
.balign 8
counter: .quad 41
arguments: .quad target, fresh, 0
.section .bss
.balign 8
wait_status: .skip 8
.section .note.GNU-stack,"",@progbits
