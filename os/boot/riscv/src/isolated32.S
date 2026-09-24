# ArchTest fixture (RV32): the "component" that runs in a
# private address space.
#
# 与 isolated64.S 同形，只有 XLEN 宽度差异（sw/lw、4 字节槽）。控制页按 usize
# 槽布局：0 magic / 1 satp / 2 resume flag / 3 iterations / 4 data。

.equ CTL_VA,   0x30000000
.equ CTL_MAGIC, 0
.equ CTL_SATP,  4
.equ CTL_FLAG,  8
.equ CTL_ITER,  12
.equ CTL_DATA,  16

.section .text.isolated_fixture,"ax"
.balign 4096
.global isolated_fixture_start
isolated_fixture_start:

# ---------------------------------------------------------------------------
# 同步往返：写 magic + 观察到的 satp，返回 0x5a
# ---------------------------------------------------------------------------
.align 2
.global isolated_roundtrip_entry
isolated_roundtrip_entry:
    li t0, CTL_VA
    li t1, 0x5151
    sw t1, CTL_MAGIC(t0)
    csrr t2, satp
    sw t2, CTL_SATP(t0)
    li a0, 0x5a
    ret

# ---------------------------------------------------------------------------
# timer：等 Core trap 路径把 resume flag 置位后返回 0x5b（超时返回 0xdead）
# ---------------------------------------------------------------------------
.align 2
.global isolated_timer_entry
isolated_timer_entry:
    li t0, CTL_VA
    li t1, 0x5b5b
    sw t1, CTL_MAGIC(t0)
    csrr t2, satp
    sw t2, CTL_SATP(t0)
    li t1, 0
    li t3, 0x04000000
.Ltimer_wait:
    lw t2, CTL_FLAG(t0)
    bnez t2, .Ltimer_resumed
    addi t1, t1, 1
    addi t3, t3, -1
    bnez t3, .Ltimer_wait
    li a0, 0xdead
    ret
.Ltimer_resumed:
    sw t1, CTL_ITER(t0)
    li a0, 0x5b
    ret

# ---------------------------------------------------------------------------
# fault：第一次 load DATA 是未映射页（load page fault）→ Core 策略补映射 →
# 恢复后重试成功；把读到的值写进控制页并返回 0x5c
# ---------------------------------------------------------------------------
.align 2
.global isolated_fault_entry
isolated_fault_entry:
    li t0, CTL_VA
    li t1, 0x5c5c
    sw t1, CTL_MAGIC(t0)
    csrr t2, satp
    sw t2, CTL_SATP(t0)
    li t2, 0x30001000
    lw t1, 0(t2)           # 未映射 → fault；policy 补映射后本指令重试成功
    sw t1, CTL_DATA(t0)
    li a0, 0x5c
    ret

# ---------------------------------------------------------------------------
# abandon：跳进未映射地址（instruction page fault）→ Core 策略拒绝恢复
# ---------------------------------------------------------------------------
.align 2
.global isolated_abandon_entry
isolated_abandon_entry:
    li t0, 0x50000000
    jr t0
    li a0, 0xdead
    ret

.global isolated_fixture_end
isolated_fixture_end:
# 页独占：对齐 + 显式多占一整页（`.balign` 不改段大小），保证首页只有夹具代码。
.balign 4096
.space 4096

# ---------------------------------------------------------------------------
# 同步往返的寄存器纪律探针（核心 AS；Core 代码）
# ---------------------------------------------------------------------------
.section .text,"ax"
.global isolated_roundtrip_probe
.align 2
isolated_roundtrip_probe:
    la t0, ISOLATED_GP_BEFORE
    sw gp, 0(t0)
    li s0, 0x101
    li s1, 0x102
    li s2, 0x103
    li s3, 0x104
    li s4, 0x105
    li s5, 0x106
    li s6, 0x107
    li s7, 0x108
    li s8, 0x109
    li s9, 0x10a
    li s10, 0x10b
    li s11, 0x10c
    li tp, 0x707
    call isolated_roundtrip_driver
    la t0, ISOLATED_S_REGS
    sw s0, 0(t0)
    sw s1, 4(t0)
    sw s2, 8(t0)
    sw s3, 12(t0)
    sw s4, 16(t0)
    sw s5, 20(t0)
    sw s6, 24(t0)
    sw s7, 28(t0)
    sw s8, 32(t0)
    sw s9, 36(t0)
    sw s10, 40(t0)
    sw s11, 44(t0)
    la t0, ISOLATED_TP_AFTER
    sw tp, 0(t0)
    li tp, 0
    la t0, ISOLATED_GP_MATCH
    la t1, ISOLATED_GP_BEFORE
    lw t2, 0(t1)
    xor t2, t2, gp
    sw t2, 0(t0)
    tail selftest_isolated_roundtrip_resumed
