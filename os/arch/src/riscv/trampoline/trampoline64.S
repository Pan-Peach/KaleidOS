# Minimal cross-address-space trampoline (RV64).
#
#   trampoline_enter(ctx: *mut Context) -> usize
#   trampoline_return(ctx: *mut Context) -> !     (never returns to its caller)
#
# It only saves/restores the synchronous C-ABI state and switches satp / stack /
# tp.  There is no trap frame, no phase machine, no stvec switch and no fault
# policy: `stvec` stays the normal Core vector, so traps inside the target AS go
# through the ordinary Core trap path (Core code/stack/globals are shared into
# every Isolated AS at the same VA -> PA).
#
# ctx offsets are the ABI (Rust `Context` asserts them).
.equ T_RA,    0
.equ T_SP,    8
.equ T_GP,    16
.equ T_TP,    24
.equ T_S,     32            # s0 .. s11 (12 words)
.equ T_SSTATUS, 128
.equ T_SATP,  136
.equ T_INSTANCE_SATP, 144
.equ T_ENTRY, 152
.equ T_STACK, 160
.equ T_IRQ,   168
.equ T_ARG0,  176
.equ T_ARG1,  184
.equ T_ARG2,  192
.equ T_ARG3,  200
.equ T_RESULT, 208
.equ T_A0,    216
.equ SIE_BIT, 2

.section .text.trampoline,"ax"
.align 2

# ---------------------------------------------------------------------------
# 同步进入：保存调用者现场 → 切换激活 / 栈 / tp → 调入口
# ---------------------------------------------------------------------------
.global trampoline_enter
trampoline_enter:
    sd ra, T_RA(a0)
    sd sp, T_SP(a0)
    sd gp, T_GP(a0)
    sd tp, T_TP(a0)
    sd s0, T_S+0(a0)
    sd s1, T_S+8(a0)
    sd s2, T_S+16(a0)
    sd s3, T_S+24(a0)
    sd s4, T_S+32(a0)
    sd s5, T_S+40(a0)
    sd s6, T_S+48(a0)
    sd s7, T_S+56(a0)
    sd s8, T_S+64(a0)
    sd s9, T_S+72(a0)
    sd s10, T_S+80(a0)
    sd s11, T_S+88(a0)

    csrr t0, sstatus
    sd t0, T_SSTATUS(a0)
    csrr t0, satp
    sd t0, T_SATP(a0)

    # 只在激活不同时切 satp + 全量 sfence.vma。
    ld t1, T_INSTANCE_SATP(a0)
    beq t0, t1, 1f
    csrw satp, t1
    sfence.vma
1:
    # s0 = ctx（组件按 C ABI 必须保持 s0；返回点据此写入结果）。
    mv s0, a0
    ld sp, T_STACK(s0)
    # 新执行显式清零 tp：跨 AS 进入不继承挂起调用者的 tp（本镜像无 TLS；
    # tp 只是被透明保存 / 恢复的架构执行状态）。调用者 tp 已在上面保存。
    li tp, 0
    ld t0, T_IRQ(s0)
    beqz t0, 2f
    csrs sstatus, SIE_BIT
2:
    ld t2, T_ENTRY(s0)
    ld a1, T_ARG1(s0)
    ld a2, T_ARG2(s0)
    ld a3, T_ARG3(s0)
    ld a0, T_ARG0(s0)
    jalr t2

# ---------------------------------------------------------------------------
# 组件正常返回（ra 由上面的 jalr 设置）
# ---------------------------------------------------------------------------
    li t0, 0
    sd t0, T_RESULT(s0)
    sd a0, T_A0(s0)
    j .Lrestore

# ---------------------------------------------------------------------------
# Core trap 路径放弃组件：a0 = ctx；交回挂起的 Core 调用者
# ---------------------------------------------------------------------------
.global trampoline_return
trampoline_return:
    mv s0, a0
    li t0, 1
    sd t0, T_RESULT(s0)
    sd zero, T_A0(s0)

.Lrestore:
    # 恢复调用者激活（不同才切 + sfence）。
    csrr t0, satp
    ld t1, T_SATP(s0)
    beq t0, t1, 3f
    csrw satp, t1
    sfence.vma
3:
    # 显式恢复中断状态；再由调用者现场恢复寄存器（与进入时严格对称）。
    ld t0, T_SSTATUS(s0)
    csrw sstatus, t0
    ld gp, T_GP(s0)
    ld tp, T_TP(s0)
    ld ra, T_RA(s0)
    ld t3, T_SP(s0)
    ld s1, T_S+8(s0)
    ld s2, T_S+16(s0)
    ld s3, T_S+24(s0)
    ld s4, T_S+32(s0)
    ld s5, T_S+40(s0)
    ld s6, T_S+48(s0)
    ld s7, T_S+56(s0)
    ld s8, T_S+64(s0)
    ld s9, T_S+72(s0)
    ld s10, T_S+80(s0)
    ld s11, T_S+88(s0)
    ld t2, T_S+0(s0)
    mv sp, t3
    mv s0, t2
    ret
