/* Run only in a fresh 128 MiB KaleidOS guest, not the ordinary exec corpus.
 * Exhaust brk backing, then check mprotect fails without changing permissions
 * or killing Core. Coarse growth followed by pages also pressures map metadata.
 */
.section .text.start
.global _start
_start:
    li a0, 0
    li a7, 214
    ecall
    mv s0, a0
    li s1, 0x100000
grow:
    add s2, s0, s1
    mv a0, s2
    li a7, 214
    ecall
    bne a0, s2, small
    mv s0, a0
    j grow
small:
    li t0, 4096
    beq s1, t0, protect
    mv s1, t0
    j grow
protect:
    la a0, value
    li a1, 4096
    li a2, 1
    li a7, 226
    ecall
    li t0, -12 /* ENOMEM */
    bne a0, t0, fail
    la t0, value
    li t1, 42
    sd t1, 0(t0) /* Rejected mprotect must leave the old RW mapping intact. */
    li a0, 1
    la a1, message
    li a2, 14
    li a7, 64
    ecall
    li t0, 14
    bne a0, t0, fail
    li a0, 0
    j exit
fail:
    li a0, 17
exit:
    li a7, 93
    ecall
.section .rodata
message: .ascii "OOM_RECOVERED\n"
.section .bss
.balign 4096
value: .skip 4096
