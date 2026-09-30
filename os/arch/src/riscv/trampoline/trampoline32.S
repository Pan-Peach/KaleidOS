# Minimal cross-address-space trampoline (RV32).
#
# Same mechanism / ABI as trampoline64.S, only XLEN-width differences (sw/lw,
# 4-byte slot words).  satp is a 32-bit Sv32 value.
#
# ctx offsets are the ABI (Rust `Context` asserts them).
.equ T_RA,    0
.equ T_SP,    4
.equ T_GP,    8
.equ T_TP,    12
.equ T_S,     16            # s0 .. s11 (12 words)
.equ T_SSTATUS, 64
.equ T_SATP,  68
.equ T_INSTANCE_SATP, 72
.equ T_ENTRY, 76
.equ T_STACK, 80
.equ T_IRQ,   84
.equ T_ARG0,  88
.equ T_ARG1,  92
.equ T_ARG2,  96
.equ T_ARG3,  100
.equ T_RESULT, 104
.equ T_A0,    108
.equ SIE_BIT, 2

.section .text.trampoline,"ax"
.align 2

# ---------------------------------------------------------------------------
# 同步进入：保存调用者现场 → 切换激活 / 栈 / tp → 调入口
# ---------------------------------------------------------------------------
.global trampoline_enter
trampoline_enter:
    sw ra, T_RA(a0)
    sw sp, T_SP(a0)
    sw gp, T_GP(a0)
    sw tp, T_TP(a0)
    sw s0, T_S+0(a0)
    sw s1, T_S+4(a0)
    sw s2, T_S+8(a0)
    sw s3, T_S+12(a0)
    sw s4, T_S+16(a0)
    sw s5, T_S+20(a0)
    sw s6, T_S+24(a0)
    sw s7, T_S+28(a0)
    sw s8, T_S+32(a0)
    sw s9, T_S+36(a0)
    sw s10, T_S+40(a0)
    sw s11, T_S+44(a0)

    csrr t0, sstatus
    sw t0, T_SSTATUS(a0)
    csrr t0, satp
    sw t0, T_SATP(a0)

    # 只在激活不同时切 satp + 全量 sfence.vma。
    lw t1, T_INSTANCE_SATP(a0)
    beq t0, t1, 1f
    csrw satp, t1
    sfence.vma
1:
    mv s0, a0
    lw sp, T_STACK(s0)
    # 新执行显式清零 tp：跨 AS 进入不继承挂起调用者的 tp（本镜像无 TLS；
    # tp 只是被透明保存 / 恢复的架构执行状态）。调用者 tp 已在上面保存。
    li tp, 0
    lw t0, T_IRQ(s0)
    beqz t0, 2f
    csrs sstatus, SIE_BIT
2:
    lw t2, T_ENTRY(s0)
    lw a1, T_ARG1(s0)
    lw a2, T_ARG2(s0)
    lw a3, T_ARG3(s0)
    lw a0, T_ARG0(s0)
    jalr t2

# ---------------------------------------------------------------------------
# 组件正常返回（ra 由上面的 jalr 设置）
# ---------------------------------------------------------------------------
    li t0, 0
    sw t0, T_RESULT(s0)
    sw a0, T_A0(s0)
    j .Lrestore

# ---------------------------------------------------------------------------
# Core trap 路径放弃组件：a0 = ctx；交回挂起的 Core 调用者
# ---------------------------------------------------------------------------
.global trampoline_return
trampoline_return:
    mv s0, a0
    li t0, 1
    sw t0, T_RESULT(s0)
    sw zero, T_A0(s0)

.Lrestore:
    csrr t0, satp
    lw t1, T_SATP(s0)
    beq t0, t1, 3f
    csrw satp, t1
    sfence.vma
3:
    lw t0, T_SSTATUS(s0)
    csrw sstatus, t0
    lw gp, T_GP(s0)
    lw tp, T_TP(s0)
    lw ra, T_RA(s0)
    lw t3, T_SP(s0)
    lw s1, T_S+4(s0)
    lw s2, T_S+8(s0)
    lw s3, T_S+12(s0)
    lw s4, T_S+16(s0)
    lw s5, T_S+20(s0)
    lw s6, T_S+24(s0)
    lw s7, T_S+28(s0)
    lw s8, T_S+32(s0)
    lw s9, T_S+36(s0)
    lw s10, T_S+40(s0)
    lw s11, T_S+44(s0)
    lw t2, T_S+0(s0)
    mv sp, t3
    mv s0, t2
    ret
