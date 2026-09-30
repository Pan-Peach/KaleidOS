# AP（次 CPU）入口 trampoline —— SMP 骨架，步骤体待手写（人类实现）。
#
# 由 SBI HSM `hart_start` 进入，入口环境由固件固定：
#   pc            = 本符号的**物理地址**（= 链接 LMA；见 smp.rs::secondary_entry_address）
#   a0            = hartid
#   a1            = &ApBoot（**物理**地址；布局见 src/smp.rs）
#   satp          = 0        （MMU 关闭 —— 因此下面全程只能用物理地址）
#   sstatus.SIE   = 0        （本地中断关闭）
#
# 它放在**正式 `.text`**（高 VMA、低 LMA）：AP 从它的 LMA（= 物理地址）执行；而
# 高半区代码引用它时是高→高 PC 相对，不会 PCREL 超范围。它**不能**进
# `.text.entry`——那会与 `_start` 争抢镜像基址（`_start` 用 `auipc` 把自身地址
# 当 kernel_pa）。
#
# 目标：把 AP 从物理世界带进长期内核地址空间（satp → 高半区），再跳进高半区的
#       Rust 入口 secondary_main(argument)。
#
# 与 src/smp.rs 的 `ApBoot` 偏移绑定（word = 8 字节，RV64）：
#   ApBoot.boot_stack_top   = 0   进入 Rust 前的物理栈顶
#   ApBoot.kernel_stack_top = 8   切地址空间后的高半区内核栈顶
#   ApBoot.satp             = 16  长期内核根页表 satp 值 (MODE | PPN)
#   ApBoot.entry            = 24  高半区 Rust 入口 (secondary_main)
#   ApBoot.argument         = 32  传给入口的不透明值（逻辑 CPU 稠密下标）

.section .text
.globl _secondary_start

_secondary_start:
    #   1. 保存 a0(hartid) / a1(&ApBoot)（后面切栈/切地址空间要用）。
    ld  sp, 0(a1)        # 物理栈顶
    #   3. 切换长期地址空间：
    
    ld  t0, 16(a1)        # satp (MODE | PPN)
    csrw satp, t0
    sfence.vma
    
    #   4. 高半区栈：sp = [a1 + 8]
    ld  sp, 8(a1)
    #   4b. 进入 Rust 前清 tp：`tp = 0`（全新执行不带任何寄存器残留）。契约显式化，
    #       不依赖固件留下的寄存器值（Oracle 评审）。
    li  tp, 0
    #   5. 进入 Rust：
    ld  t0, 24(a1)        # entry = secondary_main（高 VA）
    ld  a0, 32(a1)        # argument
    jr  t0                # 永不返回 
    # 兜底：未实现时，任何早到的 AP 都停在这里（不得退回固件）。
1:
    wfi
    j 1b
