.align 2
.global trap_vec
trap_vec:
    # Stack-safe trap entry (single convention for Core and component code).
    #
    # Outside handling `sscratch` holds the safe Core trap-stack top.  On an
    # outer trap we switch to it BEFORE writing anything through the interrupted
    # `sp` (whose contents may not be valid in Core context), preserving the
    # interrupted sp in `TrapFrame.x[2]`.  While handling, `sscratch` is zero:
    # a nested trap stays on the safe stack (no switch).
    #
    # The frame is 272 bytes (TrapFrame) plus one marker word at 272 holding the
    # `sscratch` value to restore on `sret` (safe top for an outer trap, 0 for a
    # nested one).  The allocation is 288 bytes so the ABI keeps `sp` 16-byte
    # aligned.
    csrrw sp, sscratch, sp
    beqz sp, .Lnested

    # ---- outer trap: interrupted stack is not the safe trap stack ----
    addi sp, sp, -288
    sd t0, 40(sp)
    sd t1, 48(sp)
    sd t2, 56(sp)
    csrr t0, sscratch            # t0 = interrupted sp
    sd t0, 16(sp)                # frame.x[2] = interrupted sp
    addi t0, sp, 288             # t0 = safe trap-stack top
    sd t0, 272(sp)               # outer: reinstall it on sret
    csrw sscratch, zero          # nested convention while handling
    j .Lsave_rest

.Lnested:
    # ---- nested trap: already on the safe trap stack ----
    csrrw sp, sscratch, sp       # sp := interrupted sp, sscratch := 0
    addi sp, sp, -288
    sd t0, 40(sp)
    sd t1, 48(sp)
    sd t2, 56(sp)
    addi t0, sp, 288
    sd t0, 16(sp)                # frame.x[2] = interrupted sp
    sd zero, 272(sp)             # nested: stay on the safe stack

.Lsave_rest:
    sd ra, 8(sp)
    sd gp, 24(sp)
    sd tp, 32(sp)
    sd s0, 64(sp)
    sd s1, 72(sp)
    sd a0, 80(sp)
    sd a1, 88(sp)
    sd a2, 96(sp)
    sd a3, 104(sp)
    sd a4, 112(sp)
    sd a5, 120(sp)
    sd a6, 128(sp)
    sd a7, 136(sp)
    sd s2, 144(sp)
    sd s3, 152(sp)
    sd s4, 160(sp)
    sd s5, 168(sp)
    sd s6, 176(sp)
    sd s7, 184(sp)
    sd s8, 192(sp)
    sd s9, 200(sp)
    sd s10, 208(sp)
    sd s11, 216(sp)
    sd t3, 224(sp)
    sd t4, 232(sp)
    sd t5, 240(sp)
    sd t6, 248(sp)

    csrr t0, sstatus
    sd t0, 256(sp)
    csrr t0, sepc
    sd t0, 264(sp)

    csrr a1, scause
    csrr a2, stval
    mv a0, sp
    call trap_handler

    ld t0, 264(sp)
    csrw sepc, t0
    ld t0, 256(sp)
    csrw sstatus, t0

    ld ra, 8(sp)
    ld gp, 24(sp)
    ld tp, 32(sp)
    ld t0, 40(sp)
    ld t1, 48(sp)
    ld t2, 56(sp)
    ld s0, 64(sp)
    ld s1, 72(sp)
    ld a0, 80(sp)
    ld a1, 88(sp)
    ld a2, 96(sp)
    ld a3, 104(sp)
    ld a4, 112(sp)
    ld a5, 120(sp)
    ld a6, 128(sp)
    ld a7, 136(sp)
    ld s2, 144(sp)
    ld s3, 152(sp)
    ld s4, 160(sp)
    ld s5, 168(sp)
    ld s6, 176(sp)
    ld s7, 184(sp)
    ld s8, 192(sp)
    ld s9, 200(sp)
    ld s10, 208(sp)
    ld s11, 216(sp)
    ld t3, 224(sp)
    ld t4, 232(sp)
    ld t5, 240(sp)
    ld t6, 248(sp)

    # Restore the sscratch convention: safe top after an outer trap, 0 while
    # nested handling continues, then the interrupted sp / t0.
    ld t0, 272(sp)
    csrw sscratch, t0
    ld t0, 40(sp)
    ld sp, 16(sp)
    sret
