.align 2
.global trap_vec
trap_vec:
    # 统一约定（rv64）：`sscratch` 恒 = 本 CPU 的 `CpuEntry*`。
    #
    # `PerCpu` 把 `entry` 紧贴在它的 trap 栈之上，所以这个指针**同时**是
    # 「入口记录指针」和「安全 trap 栈顶」——一个寄存器够用，不需要任何临时
    # 寄存器。UP 与 SMP 走同一条路径（UP = 只有 CPU0 的 SMP）。
    #
    # 换栈：sp := &entry（即 trap 栈顶），sscratch := 被打断的 sp。
    csrrw sp, sscratch, sp
    beqz sp, .Lunbound          # sscratch == 0 => 本 CPU 未绑定（不该发生）

    # 在 trap 栈上分配 TrapFrame（272 字节）+ 16 对齐 = 288 字节。
    addi sp, sp, -288
    sd t0, 40(sp)
    sd t1, 48(sp)
    sd t2, 56(sp)
    csrr t0, sscratch           # t0 = 被打断的 sp
    sd t0, 16(sp)               # frame.x[2] = 被打断的 sp
    addi t0, sp, 288            # t0 = &entry（== trap 栈顶）
    csrw sscratch, t0           # 恢复 sscratch = &entry（处理期间保持稳定）

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

    # sscratch 全程未变（= &entry），无需恢复；直接还原 sp / sret。
    ld sp, 16(sp)
    sret

.Lunbound:
    wfi
    j .Lunbound
