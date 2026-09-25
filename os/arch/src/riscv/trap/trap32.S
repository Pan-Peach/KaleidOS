.align 2
.global trap_vec
trap_vec:
    # Stack-safe trap entry (RV32).  Same convention as trap64.S:
    # outside handling `sscratch` holds the safe Core trap-stack top; while
    # handling it is zero (nested traps stay on the safe stack).
    #
    # TrapFrame occupies 136 bytes; the word at 136 stores the `sscratch` value
    # to restore on `sret`.  The allocation is 144 bytes so `sp` stays 16-byte
    # aligned for the RV32 ABI.
    csrrw sp, sscratch, sp
    beqz sp, .Lnested

    # ---- outer trap: interrupted stack is not the safe trap stack ----
    addi sp, sp, -144
    sw t0, 20(sp)
    sw t1, 24(sp)
    sw t2, 28(sp)
    csrr t0, sscratch            # t0 = interrupted sp
    sw t0, 8(sp)                 # frame.x[2] = interrupted sp
    addi t0, sp, 144             # t0 = safe trap-stack top
    sw t0, 136(sp)               # outer: reinstall it on sret
    csrw sscratch, zero          # nested convention while handling
    j .Lsave_rest

.Lnested:
    # ---- nested trap: already on the safe trap stack ----
    csrrw sp, sscratch, sp       # sp := interrupted sp, sscratch := 0
    addi sp, sp, -144
    sw t0, 20(sp)
    sw t1, 24(sp)
    sw t2, 28(sp)
    addi t0, sp, 144
    sw t0, 8(sp)                 # frame.x[2] = interrupted sp
    sw zero, 136(sp)             # nested: stay on the safe stack

.Lsave_rest:
    sw ra, 4(sp)
    sw gp, 12(sp)
    sw tp, 16(sp)
    sw s0, 32(sp)
    sw s1, 36(sp)
    sw a0, 40(sp)
    sw a1, 44(sp)
    sw a2, 48(sp)
    sw a3, 52(sp)
    sw a4, 56(sp)
    sw a5, 60(sp)
    sw a6, 64(sp)
    sw a7, 68(sp)
    sw s2, 72(sp)
    sw s3, 76(sp)
    sw s4, 80(sp)
    sw s5, 84(sp)
    sw s6, 88(sp)
    sw s7, 92(sp)
    sw s8, 96(sp)
    sw s9, 100(sp)
    sw s10, 104(sp)
    sw s11, 108(sp)
    sw t3, 112(sp)
    sw t4, 116(sp)
    sw t5, 120(sp)
    sw t6, 124(sp)

    csrr t0, sstatus
    sw t0, 128(sp)
    csrr t0, sepc
    sw t0, 132(sp)

    csrr a1, scause
    csrr a2, stval
    mv a0, sp
    call trap_handler

    lw t0, 132(sp)
    csrw sepc, t0
    lw t0, 128(sp)
    csrw sstatus, t0

    lw ra, 4(sp)
    lw gp, 12(sp)
    lw tp, 16(sp)
    lw t0, 20(sp)
    lw t1, 24(sp)
    lw t2, 28(sp)
    lw s0, 32(sp)
    lw s1, 36(sp)
    lw a0, 40(sp)
    lw a1, 44(sp)
    lw a2, 48(sp)
    lw a3, 52(sp)
    lw a4, 56(sp)
    lw a5, 60(sp)
    lw a6, 64(sp)
    lw a7, 68(sp)
    lw s2, 72(sp)
    lw s3, 76(sp)
    lw s4, 80(sp)
    lw s5, 84(sp)
    lw s6, 88(sp)
    lw s7, 92(sp)
    lw s8, 96(sp)
    lw s9, 100(sp)
    lw s10, 104(sp)
    lw s11, 108(sp)
    lw t3, 112(sp)
    lw t4, 116(sp)
    lw t5, 120(sp)
    lw t6, 124(sp)

    # Restore the sscratch convention, then the interrupted sp / t0.
    lw t0, 136(sp)
    csrw sscratch, t0
    lw t0, 20(sp)
    lw sp, 8(sp)
    sret
