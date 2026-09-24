# KaleidOS increment 3 ArchTest fixture (RV64): the "component" that runs in a
# private address space.
#
# 白色盒测试夹具：每个入口都只在**实例 AS** 里执行，只访问实例 AS 映射的 VA。
# 整页由测试映射进实例 AS（同 VA → 同 PA），`.balign 4096` 起止保证页独占。
#
#   CTL  = 0x30000000  控制页（RW；Core 通过 identity PA 视图观察）
#   DATA = 0x30001000  惰性数据页（初始不映射；fault 用例由 Core 策略补映射）
#
# 控制页按 usize 槽布局（本文件固定 8 字节槽；Rust 侧用槽号索引）：
#   0  magic            1 satp（组件观察到的）  2 resume flag
#   3  loop iterations  4 data（补映射后读回）

.equ CTL_VA,   0x30000000
.equ CTL_MAGIC, 0
.equ CTL_SATP,  8
.equ CTL_FLAG,  16
.equ CTL_ITER,  24
.equ CTL_DATA,  32

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
    sd t1, CTL_MAGIC(t0)
    csrr t2, satp
    sd t2, CTL_SATP(t0)
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
    sd t1, CTL_MAGIC(t0)
    csrr t2, satp
    sd t2, CTL_SATP(t0)
    li t1, 0
    li t3, 0x04000000
.Ltimer_wait:
    ld t2, CTL_FLAG(t0)
    bnez t2, .Ltimer_resumed
    addi t1, t1, 1
    addi t3, t3, -1
    bnez t3, .Ltimer_wait
    li a0, 0xdead
    ret
.Ltimer_resumed:
    sd t1, CTL_ITER(t0)
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
    sd t1, CTL_MAGIC(t0)
    csrr t2, satp
    sd t2, CTL_SATP(t0)
    li t2, 0x30001000
    ld t1, 0(t2)           # 未映射 → fault；policy 补映射后本指令重试成功
    sd t1, CTL_DATA(t0)
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
# 同步往返的寄存器纪律探针（核心 AS；Core 代码）：
# 设 s0-s11 / tp 为魔数 → 调 driver（内部走 gateway + 组件）→ 读回并存静态。
# gp 只做"保存 / 比较"（本镜像没有 __global_pointer$，不故意设置它）。
# ---------------------------------------------------------------------------
.section .text,"ax"
.global isolated_roundtrip_probe
.align 2
isolated_roundtrip_probe:
    la t0, ISOLATED_GP_BEFORE
    sd gp, 0(t0)
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
    sd s0, 0(t0)
    sd s1, 8(t0)
    sd s2, 16(t0)
    sd s3, 24(t0)
    sd s4, 32(t0)
    sd s5, 40(t0)
    sd s6, 48(t0)
    sd s7, 56(t0)
    sd s8, 64(t0)
    sd s9, 72(t0)
    sd s10, 80(t0)
    sd s11, 88(t0)
    la t0, ISOLATED_TP_AFTER
    sd tp, 0(t0)
    li tp, 0
    la t0, ISOLATED_GP_MATCH
    la t1, ISOLATED_GP_BEFORE
    ld t2, 0(t1)
    xor t2, t2, gp
    sd t2, 0(t0)
    tail selftest_isolated_roundtrip_resumed
