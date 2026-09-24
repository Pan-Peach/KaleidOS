# KaleidOS increment 3: the Isolated-domain assembly gateway (RV32).
#
# 与 gateway64.S 同一机制、同一 ABI，只有 XLEN 宽度差异：
#
#   gateway_enter(scratch=a0, satp=a1, entry=a2, stack_top=a3,
#                 slot=a4, irq_enable=a5, token=a6) -> void
#
# 组件入口 a0 / a1 由 Rust 包装预置在 scratch 的 G_ENTRY_ARG0 / G_ENTRY_ARG1
# （入口 ABI 由 Core 解释，汇编只搬运）。
#
# 偏移常量必须与 `GatewayScratch` 字段偏移一致（Rust 侧 const 断言钉住字段
# 偏移；QEMU ArchTest 钉住本文件的实际行为）。`entry_stvec` / `core_stvec` /
# `trap_stack_top` 由 Rust 包装在调用前写好。
#
# RV32 的 satp 是 32 位（Sv32 MODE=1），整字比较 / 装载；其余纪律与 RV64 相同。

.equ G_FRAME,             0
.equ G_FRAME_A0,          40      # frame.x[10]：组件返回值暂存
.equ G_FRAME_STATUS,      128
.equ G_FRAME_EPC,         132
.equ G_CORE_RA,           136
.equ G_CORE_SP,           140
.equ G_CORE_GP,           144
.equ G_CORE_TP,           148
.equ G_CORE_S,            152     # s0 .. s11（12 个字，连续）
.equ G_CORE_SSTATUS,      200
.equ G_CORE_SATP,         204
.equ G_CORE_STVEC,        208
.equ G_CORE_SSCRATCH,     212
.equ G_CORE_TRAP_SP,      216
.equ G_INSTANCE_SATP,     220
.equ G_ENTRY_STVEC,       224
.equ G_PHASE,             228
.equ G_STATUS,            232
.equ G_TOKEN,             236
.equ G_ENTRY_ARG0,        240     # 组件入口 a0（Core 预置，见 Transition::arg0）
.equ G_ENTRY_ARG1,        244     # 组件入口 a1（Core 预置，见 Transition::arg1）

.equ PHASE_TO_COMPONENT,  1
.equ PHASE_COMPONENT,     2
.equ PHASE_TO_CORE,       3
.equ SIE_BIT,             2

.section .text.gateway,"ax"
.balign 4096
.global gateway_code_start
gateway_code_start:

# ---------------------------------------------------------------------------
# 同步进入：Core -> 私有 AS
# ---------------------------------------------------------------------------
.align 2
.global gateway_enter
gateway_enter:
    # 挂起 Core 现场（callee-saved + gp/tp/ra/sp + sstatus/satp/stvec/sscratch）。
    sw ra, G_CORE_RA(a0)
    sw sp, G_CORE_SP(a0)
    sw gp, G_CORE_GP(a0)
    sw tp, G_CORE_TP(a0)
    sw s0, G_CORE_S+0(a0)
    sw s1, G_CORE_S+4(a0)
    sw s2, G_CORE_S+8(a0)
    sw s3, G_CORE_S+12(a0)
    sw s4, G_CORE_S+16(a0)
    sw s5, G_CORE_S+20(a0)
    sw s6, G_CORE_S+24(a0)
    sw s7, G_CORE_S+28(a0)
    sw s8, G_CORE_S+32(a0)
    sw s9, G_CORE_S+36(a0)
    sw s10, G_CORE_S+40(a0)
    sw s11, G_CORE_S+44(a0)

    csrr t0, sstatus
    sw t0, G_CORE_SSTATUS(a0)
    csrr t0, satp
    sw t0, G_CORE_SATP(a0)
    csrr t0, stvec
    sw t0, G_CORE_STVEC(a0)
    csrr t0, sscratch
    sw t0, G_CORE_SSCRATCH(a0)
    sw a1, G_INSTANCE_SATP(a0)
    sw a6, G_TOKEN(a0)

    li t0, PHASE_TO_COMPONENT
    sw t0, G_PHASE(a0)
    # trap 向量切到双映射的 gateway 入口；sscratch = scratch（Core 独占）。
    lw t0, G_ENTRY_STVEC(a0)
    csrw stvec, t0
    csrw sscratch, a0

    # 切根。此后只碰双映射 scratch / 组件栈，直到 Core root 恢复。
    csrw satp, a1
    sfence.vma
    li t0, PHASE_COMPONENT
    sw t0, G_PHASE(a0)

    # 组件初始上下文：sp / tp / 中断使能。
    mv sp, a3
    mv tp, a4
    beqz a5, 1f
    csrs sstatus, SIE_BIT
1:
    # 组件入口参数（Core 预置在 scratch；此刻 a0 = &scratch，sscratch 已持有它）。
    lw t0, G_ENTRY_ARG0(a0)
    lw a1, G_ENTRY_ARG1(a0)
    mv a0, t0
    jalr a2                      # ra = 紧随其后的返回点

# ---------------------------------------------------------------------------
# 同步返回：组件 `ret` 落在这里（ra 由上面的 jalr 设置）
# ---------------------------------------------------------------------------
    # 关中断：退出窗口 phase 已是 TO_CORE，任何 trap 都只能是 broken transition。
    csrr t0, sscratch            # t0 = &scratch（Core 独占的 sscratch）
    csrci sstatus, SIE_BIT
    sw a0, G_FRAME_A0(t0)        # 暂存组件返回值
    li t1, PHASE_TO_CORE
    sw t1, G_PHASE(t0)

    lw t1, G_CORE_SATP(t0)
    csrw satp, t1
    sfence.vma
    lw t1, G_CORE_STVEC(t0)
    csrw stvec, t1
    lw t1, G_CORE_SSCRATCH(t0)
    csrw sscratch, t1
    lw t1, G_CORE_SSTATUS(t0)
    csrw sstatus, t1

    # 恢复 Core callee-saved；s0 是 scratch base，最后恢复。
    lw gp, G_CORE_GP(t0)
    lw tp, G_CORE_TP(t0)
    lw ra, G_CORE_RA(t0)
    lw t1, G_CORE_SP(t0)
    lw s1, G_CORE_S+4(t0)
    lw s2, G_CORE_S+8(t0)
    lw s3, G_CORE_S+12(t0)
    lw s4, G_CORE_S+16(t0)
    lw s5, G_CORE_S+20(t0)
    lw s6, G_CORE_S+24(t0)
    lw s7, G_CORE_S+28(t0)
    lw s8, G_CORE_S+32(t0)
    lw s9, G_CORE_S+36(t0)
    lw s10, G_CORE_S+40(t0)
    lw s11, G_CORE_S+44(t0)
    lw t2, G_CORE_S+0(t0)
    li t3, 0
    sw t3, G_STATUS(t0)          # status = 0：组件正常返回
    mv sp, t1
    mv s0, t2
    ret

# ---------------------------------------------------------------------------
# 组件上下文 trap 入口（组件运行期 stvec 指向这里）
# ---------------------------------------------------------------------------
.align 2
.global gateway_trap_entry
gateway_trap_entry:
    # sscratch = &scratch。交换出被打断的组件 sp（不碰组件 sp 指向的内存）。
    csrrw sp, sscratch, sp

    # 完整被打断现场（与 `TrapFrame` 逐字同形）。
    sw ra, 4(sp)
    sw gp, 12(sp)
    sw tp, 16(sp)
    sw t0, 20(sp)
    sw t1, 24(sp)
    sw t2, 28(sp)
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

    csrr t0, sscratch
    sw t0, 8(sp)                 # frame.x[2] = 被打断的组件 sp
    csrr t0, sstatus
    sw t0, G_FRAME_STATUS(sp)
    csrr t0, sepc
    sw t0, G_FRAME_EPC(sp)

    # 只有“组件确实在运行”才交给 Core；过渡窗口 / 非组件上下文一律 fatal。
    lw t0, G_PHASE(sp)
    li t1, PHASE_COMPONENT
    bne t0, t1, gateway_fatal_entry

    mv s0, sp                    # s0 = &scratch（callee-saved，跨 Rust 调用）
    lw t0, G_CORE_SATP(s0)
    csrw satp, t0
    sfence.vma
    lw gp, G_CORE_GP(s0)         # Core Rust 代码必须跑在 Core 的 gp 环境下
    lw tp, G_CORE_TP(s0)
    lw sp, G_CORE_TRAP_SP(s0)    # 专用 Core trap 栈（只在 Core root 下使用）
    lw t0, G_CORE_STVEC(s0)
    csrw stvec, t0               # 嵌套 trap 回普通 Core 向量
    lw t0, G_CORE_SSCRATCH(s0)
    csrw sscratch, t0

    mv a0, s0
    csrr a1, scause
    csrr a2, stval
    lw a3, G_TOKEN(s0)
    call gateway_trap_dispatch   # a0 = 0 放弃组件 / 1 恢复组件
    beqz a0, gateway_abandon

    # ---- 恢复组件：切回实例 root（policy 可能刚往该 root 里加了映射）----
    lw t0, G_INSTANCE_SATP(s0)
    csrw satp, t0
    sfence.vma
    lw t0, G_ENTRY_STVEC(s0)
    csrw stvec, t0
    csrw sscratch, s0

    # 与 trap32.S 同款的恢复顺序（sp = &scratch；t0 最后恢复）。
    mv sp, s0
    lw t0, G_FRAME_EPC(sp)
    csrw sepc, t0
    lw t0, G_FRAME_STATUS(sp)
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
    lw sp, 8(sp)
    sret

# ---------------------------------------------------------------------------
# 放弃组件：回到挂起的 Core 调用者（s0 = &scratch；Core root 已生效）
# ---------------------------------------------------------------------------
.align 2
gateway_abandon:
    lw t0, G_CORE_STVEC(s0)
    csrw stvec, t0
    lw t0, G_CORE_SSCRATCH(s0)
    csrw sscratch, t0
    lw t0, G_CORE_SSTATUS(s0)
    csrw sstatus, t0
    li t0, 1
    sw t0, G_STATUS(s0)          # status = 1：Core 判定组件不可恢复

    lw gp, G_CORE_GP(s0)
    lw tp, G_CORE_TP(s0)
    lw ra, G_CORE_RA(s0)
    lw t1, G_CORE_SP(s0)
    lw s1, G_CORE_S+4(s0)
    lw s2, G_CORE_S+8(s0)
    lw s3, G_CORE_S+12(s0)
    lw s4, G_CORE_S+16(s0)
    lw s5, G_CORE_S+20(s0)
    lw s6, G_CORE_S+24(s0)
    lw s7, G_CORE_S+28(s0)
    lw s8, G_CORE_S+32(s0)
    lw s9, G_CORE_S+36(s0)
    lw s10, G_CORE_S+40(s0)
    lw s11, G_CORE_S+44(s0)
    lw t2, G_CORE_S+0(s0)
    mv sp, t1
    mv s0, t2
    ret

# ---------------------------------------------------------------------------
# Broken transition / 非组件上下文 trap：尽力切回 Core root，然后 panic
# ---------------------------------------------------------------------------
.align 2
gateway_fatal_entry:
    mv a0, t0                    # phase（观察值）
    csrr a1, scause
    csrr a2, sepc
    csrr a3, stval
    mv s0, sp
    lw t1, G_CORE_SATP(s0)
    csrw satp, t1
    sfence.vma
    lw gp, G_CORE_GP(s0)
    lw sp, G_CORE_TRAP_SP(s0)
    lw t1, G_CORE_STVEC(s0)
    csrw stvec, t1
    lw t1, G_CORE_SSCRATCH(s0)
    csrw sscratch, t1
    call gateway_fatal
1:
    wfi
    j 1b

.global gateway_code_end
gateway_code_end:
.balign 4096
.space 4096
