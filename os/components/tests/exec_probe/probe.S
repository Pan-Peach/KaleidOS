/* Ordinary RV64 Linux ET_EXEC test programs; never linked as .kcomp.
 * Linux asm-generic syscall ABI: write=64, exit=93, a7=number, a0..a5=args.
 * Integer-only probes deliberately precede libc/TLS/FPU startup.
 */
.section .rodata
message:
    .ascii "EXEC_WRITE_OK\n"
message_end:
.equ message_size, message_end - message
argument:
    .asciz "probe"

.section .text
.global _start
_start:
#if defined(PROBE_EXIT_ZERO)
    li a0, 0
#elif defined(PROBE_EXIT_SEVEN)
    li a0, 7
#elif defined(PROBE_WRITE)
    li a0, 1
    la a1, message
    li a2, message_size
    li a7, 64
    ecall
    li t0, message_size
    bne a0, t0, failed
    li a0, 0
#elif defined(PROBE_STACK_BSS)
    /* Stack alignment, argc=2, argv[1]="probe", argv terminator. */
    andi t0, sp, 15
    bnez t0, failed
    ld t0, 0(sp)
    li t1, 2
    bne t0, t1, failed
    ld t0, 8(sp)
    beqz t0, failed
    lbu t1, 0(t0)
    beqz t1, failed
    ld t0, 16(sp)
    beqz t0, failed
    la t1, argument
1:
    lbu t2, 0(t0)
    lbu t3, 0(t1)
    bne t2, t3, failed
    beqz t2, 2f
    addi t0, t0, 1
    addi t1, t1, 1
    j 1b
2:
    ld t0, 24(sp)
    bnez t0, failed
    /* Walk envp then require AT_PAGESZ=4096 before AT_NULL. */
    addi t0, sp, 32
3:
    ld t1, 0(t0)
    addi t0, t0, 8
    bnez t1, 3b
4:
    ld t1, 0(t0)
    beqz t1, failed
    li t2, 6
    beq t1, t2, 5f
    addi t0, t0, 16
    j 4b
5:
    ld t1, 8(t0)
    li t2, 4096
    bne t1, t2, failed
    /* BSS must be zero across the page boundary and remain writable. */
    la t0, zero_area
    li t1, 4097
6:
    lbu t2, 0(t0)
    bnez t2, failed
    addi t0, t0, 1
    addi t1, t1, -1
    bnez t1, 6b
    li t1, 42
    sb t1, -1(t0)
    lbu t2, -1(t0)
    bne t1, t2, failed
    li a0, 0
#elif defined(PROBE_BAD_POINTER)
    /* A bad userspace pointer must yield -EFAULT, without killing Core. */
    li a0, 1
    li a1, -1
    li a2, 16
    li a7, 64
    ecall
    li t0, -14
    bne a0, t0, failed
    li a0, 1
    li a1, 0x80200000
    li a2, 4
    li a7, 64
    ecall
    li t0, -14
    bne a0, t0, failed
    la a1, copy_guard+4094
    li t0, 42
    sb t0, 0(a1)
    sb t0, 1(a1)
    li a0, 1
    li a2, 4
    li a7, 64
    ecall
    li t0, -14
    bne a0, t0, failed
    li a0, 0
#elif defined(PROBE_CORE_READ)
    li t0, 0x80200000
    ld t1, 0(t0)
    li a0, 99
#elif defined(PROBE_STACK_EXECUTE)
    addi t0, sp, -16
    li t1, 0x73
    sw t1, 0(t0)
    li a0, 99
    li a7, 93
    jr t0
#elif defined(PROBE_BREAKPOINT)
    ebreak
    li a0, 99
#elif defined(PROBE_PRIVILEGED)
    /* In real U-mode this must fault before reaching exit(99). */
    csrr t0, sstatus
    li a0, 99
#elif defined(PROBE_TEXT_WRITE)
    /* A writable text mapping must never count as successful exec. */
    la t0, _start
    sb zero, 0(t0)
    li a0, 99
#else
#error Select one exec probe
#endif
    li a7, 93
    ecall
    /* A returned exit syscall is always a broken execution path. */
    unimp
failed:
    li a0, 11
    li a7, 93
    ecall
    unimp

#if defined(PROBE_STACK_BSS)
.section .bss
.balign 16
zero_area:
    .skip 4097
#endif

#if defined(PROBE_BAD_POINTER)
.section .bss
.balign 4096
copy_guard: .skip 4096
#endif

.section .note.GNU-stack,"",@progbits
