# KaleidOS increment 3: the Isolated-domain assembly gateway (RV64).
#
# 这是 Private-AS 切换的**唯一**执行出口。它只做寄存器/栈/satp 切换，不含任何
# Rust、锁或 Core 栈访问：目标 root 生效后到 Core root 恢复前，只允许触碰
# **双映射**的 gateway 代码页 / scratch 页与组件自己的栈。
#
# ABI（与 `arch::riscv::gateway` 的 Rust 侧逐字一致）：
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
# 进入前提：调用者已关中断（`sstatus.SIE=0`）。退出：恢复 Core 的
# sstatus / stvec / sscratch / satp / sp / gp / tp / ra / s0-s11 后 `ret` 回
# Rust 包装。组件返回值暂存 `frame.x[10]`；结果状态在 `G_STATUS`（0=正常返回，
# 1=Core 放弃组件）。

.equ G_FRAME,             0
.equ G_FRAME_A0,          80      # frame.x[10]：组件返回值暂存
.equ G_FRAME_STATUS,      256
.equ G_FRAME_EPC,         264
.equ G_CORE_RA,           272
.equ G_CORE_SP,           280
.equ G_CORE_GP,           288
.equ G_CORE_TP,           296
.equ G_CORE_S,            304     # s0 .. s11（12 个字，连续）
.equ G_CORE_SSTATUS,      400
.equ G_CORE_SATP,         408
.equ G_CORE_STVEC,        416
.equ G_CORE_SSCRATCH,     424
.equ G_CORE_TRAP_SP,      432
.equ G_INSTANCE_SATP,     440
.equ G_ENTRY_STVEC,       448
.equ G_PHASE,             456
.equ G_STATUS,            464
.equ G_TOKEN,             472
.equ G_ENTRY_ARG0,        480     # 组件入口 a0（Core 预置，见 Transition::arg0）
.equ G_ENTRY_ARG1,        488     # 组件入口 a1（Core 预置，见 Transition::arg1）

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
    sd ra, G_CORE_RA(a0)
    sd sp, G_CORE_SP(a0)
    sd gp, G_CORE_GP(a0)
    sd tp, G_CORE_TP(a0)
    sd s0, G_CORE_S+0(a0)
    sd s1, G_CORE_S+8(a0)
    sd s2, G_CORE_S+16(a0)
    sd s3, G_CORE_S+24(a0)
    sd s4, G_CORE_S+32(a0)
    sd s5, G_CORE_S+40(a0)
    sd s6, G_CORE_S+48(a0)
    sd s7, G_CORE_S+56(a0)
    sd s8, G_CORE_S+64(a0)
    sd s9, G_CORE_S+72(a0)
    sd s10, G_CORE_S+80(a0)
    sd s11, G_CORE_S+88(a0)

    csrr t0, sstatus
    sd t0, G_CORE_SSTATUS(a0)
    csrr t0, satp
    sd t0, G_CORE_SATP(a0)
    csrr t0, stvec
    sd t0, G_CORE_STVEC(a0)
    csrr t0, sscratch
    sd t0, G_CORE_SSCRATCH(a0)
    sd a1, G_INSTANCE_SATP(a0)
    sd a6, G_TOKEN(a0)

    li t0, PHASE_TO_COMPONENT
    sd t0, G_PHASE(a0)
    # trap 向量切到双映射的 gateway 入口；sscratch = scratch（Core 独占）。
    ld t0, G_ENTRY_STVEC(a0)
    csrw stvec, t0
    csrw sscratch, a0

    # 切根。此后只碰双映射 scratch / 组件栈，直到 Core root 恢复。
    csrw satp, a1
    sfence.vma
    li t0, PHASE_COMPONENT
    sd t0, G_PHASE(a0)

    # 组件初始上下文：sp / tp / 中断使能。
    mv sp, a3
    mv tp, a4
    beqz a5, 1f
    csrs sstatus, SIE_BIT
1:
    # 组件入口参数（Core 预置在 scratch；此刻 a0 = &scratch，sscratch 已持有它）。
    ld t0, G_ENTRY_ARG0(a0)
    ld a1, G_ENTRY_ARG1(a0)
    mv a0, t0
    jalr a2                      # ra = 紧随其后的返回点

# ---------------------------------------------------------------------------
# 同步返回：组件 `ret` 落在这里（ra 由上面的 jalr 设置）
# ---------------------------------------------------------------------------
    # 关中断：退出窗口 phase 已是 TO_CORE，任何 trap 都只能是 broken transition。
    csrr t0, sscratch            # t0 = &scratch（Core 独占的 sscratch）
    csrci sstatus, SIE_BIT
    sd a0, G_FRAME_A0(t0)        # 暂存组件返回值
    li t1, PHASE_TO_CORE
    sd t1, G_PHASE(t0)

    ld t1, G_CORE_SATP(t0)
    csrw satp, t1
    sfence.vma
    ld t1, G_CORE_STVEC(t0)
    csrw stvec, t1
    ld t1, G_CORE_SSCRATCH(t0)
    csrw sscratch, t1
    ld t1, G_CORE_SSTATUS(t0)
    csrw sstatus, t1

    # 恢复 Core callee-saved；s0 是 scratch base，最后恢复。
    ld gp, G_CORE_GP(t0)
    ld tp, G_CORE_TP(t0)
    ld ra, G_CORE_RA(t0)
    ld t1, G_CORE_SP(t0)
    ld s1, G_CORE_S+8(t0)
    ld s2, G_CORE_S+16(t0)
    ld s3, G_CORE_S+24(t0)
    ld s4, G_CORE_S+32(t0)
    ld s5, G_CORE_S+40(t0)
    ld s6, G_CORE_S+48(t0)
    ld s7, G_CORE_S+56(t0)
    ld s8, G_CORE_S+64(t0)
    ld s9, G_CORE_S+72(t0)
    ld s10, G_CORE_S+80(t0)
    ld s11, G_CORE_S+88(t0)
    ld t2, G_CORE_S+0(t0)
    li t3, 0
    sd t3, G_STATUS(t0)          # status = 0：组件正常返回
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
    sd ra, 8(sp)
    sd gp, 24(sp)
    sd tp, 32(sp)
    sd t0, 40(sp)
    sd t1, 48(sp)
    sd t2, 56(sp)
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

    csrr t0, sscratch
    sd t0, 16(sp)                # frame.x[2] = 被打断的组件 sp
    csrr t0, sstatus
    sd t0, G_FRAME_STATUS(sp)
    csrr t0, sepc
    sd t0, G_FRAME_EPC(sp)

    # 只有“组件确实在运行”才交给 Core；过渡窗口 / 非组件上下文一律 fatal。
    ld t0, G_PHASE(sp)
    li t1, PHASE_COMPONENT
    bne t0, t1, gateway_fatal_entry

    mv s0, sp                    # s0 = &scratch（callee-saved，跨 Rust 调用）
    ld t0, G_CORE_SATP(s0)
    csrw satp, t0
    sfence.vma
    ld gp, G_CORE_GP(s0)         # Core Rust 代码必须跑在 Core 的 gp 环境下
    ld tp, G_CORE_TP(s0)
    ld sp, G_CORE_TRAP_SP(s0)    # 专用 Core trap 栈（只在 Core root 下使用）
    ld t0, G_CORE_STVEC(s0)
    csrw stvec, t0               # 嵌套 trap 回普通 Core 向量
    ld t0, G_CORE_SSCRATCH(s0)
    csrw sscratch, t0

    mv a0, s0
    csrr a1, scause
    csrr a2, stval
    ld a3, G_TOKEN(s0)
    call gateway_trap_dispatch   # a0 = 0 放弃组件 / 1 恢复组件
    beqz a0, gateway_abandon

    # ---- 恢复组件：切回实例 root（policy 可能刚往该 root 里加了映射）----
    ld t0, G_INSTANCE_SATP(s0)
    csrw satp, t0
    sfence.vma
    ld t0, G_ENTRY_STVEC(s0)
    csrw stvec, t0
    csrw sscratch, s0

    # 与 trap64.S 同款的恢复顺序（sp = &scratch；t0 最后恢复）。
    mv sp, s0
    ld t0, G_FRAME_EPC(sp)
    csrw sepc, t0
    ld t0, G_FRAME_STATUS(sp)
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
    ld sp, 16(sp)
    sret

# ---------------------------------------------------------------------------
# 放弃组件：回到挂起的 Core 调用者（s0 = &scratch；Core root 已生效）
# ---------------------------------------------------------------------------
.align 2
gateway_abandon:
    ld t0, G_CORE_STVEC(s0)
    csrw stvec, t0
    ld t0, G_CORE_SSCRATCH(s0)
    csrw sscratch, t0
    ld t0, G_CORE_SSTATUS(s0)
    csrw sstatus, t0
    li t0, 1
    sd t0, G_STATUS(s0)          # status = 1：Core 判定组件不可恢复

    ld gp, G_CORE_GP(s0)
    ld tp, G_CORE_TP(s0)
    ld ra, G_CORE_RA(s0)
    ld t1, G_CORE_SP(s0)
    ld s1, G_CORE_S+8(s0)
    ld s2, G_CORE_S+16(s0)
    ld s3, G_CORE_S+24(s0)
    ld s4, G_CORE_S+32(s0)
    ld s5, G_CORE_S+40(s0)
    ld s6, G_CORE_S+48(s0)
    ld s7, G_CORE_S+56(s0)
    ld s8, G_CORE_S+64(s0)
    ld s9, G_CORE_S+72(s0)
    ld s10, G_CORE_S+80(s0)
    ld s11, G_CORE_S+88(s0)
    ld t2, G_CORE_S+0(s0)
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
    ld t1, G_CORE_SATP(s0)
    csrw satp, t1
    sfence.vma
    ld gp, G_CORE_GP(s0)
    ld sp, G_CORE_TRAP_SP(s0)
    ld t1, G_CORE_STVEC(s0)
    csrw stvec, t1
    ld t1, G_CORE_SSCRATCH(s0)
    csrw sscratch, t1
    call gateway_fatal
1:
    wfi
    j 1b

.global gateway_code_end
gateway_code_end:
.balign 4096
.space 4096
