# RISC-V 上下文切换参考（Context Switch Reference）

> 归档自开源调研（rCore / xv6 / RISC-V psABI / 特权规范），用于 `os/arch` 的
> RV64 context 设计与 trap 入口汇编实现。所有出处为 git 固定 SHA，可直接溯源。

## 1. 核心结论

trap 发生在**任意指令点**，编译器没有保证任何寄存器已保存。
因此完整 trap context **必须保存 x1–x31 全部通用寄存器 + `sepc` + `sstatus`**；
唯一可跳过的是 x0（硬接线恒 0）。

函数"跨调用保留"的 ABI 约定**只在函数边界成立**，不适用于 trap 场景。

## 2. RV64 通用寄存器 ABI 表

出处：[riscv-elf-psabi-doc, riscv-cc.adoc#L16-L25](https://github.com/riscv-non-isa/riscv-elf-psabi-doc/blob/01017c343cd6d89ed4d1d568b1c75fac79d2a689/riscv-cc.adoc#L16-L25)

| 寄存器 | ABI 名 | 含义 | 跨调用保留? |
|---|---|---|---|
| x0 | `zero` | 恒 0 | —（不可变） |
| x1 | `ra` | 返回地址 | 否 |
| x2 | `sp` | 栈指针 | 是 |
| x3 | `gp` | 全局指针 | —（不可分配） |
| x4 | `tp` | 线程指针 | —（不可分配） |
| x5–x7 | `t0–t2` | 临时 | 否 |
| x8–x9 | `s0–s1`（s0=fp） | 被调用者保存 | 是 |
| x10–x17 | `a0–a7` | 参数 | 否 |
| x18–x27 | `s2–s11` | 被调用者保存 | 是 |
| x28–x31 | `t3–t6` | 临时 | 否 |

规范补充（同文件 L27-L28）："标准 ABI 中 procedure 不应修改 `tp`/`gp`，
因为 signal handler 可能依赖其值。"

## 3. CSR：要存哪些

出处：[RISC-V 特权规范, src/priv/supervisor.adoc#L39-L133](https://github.com/riscv/riscv-isa-manual/blob/07531fdab1b9e2c3c2306fd3805dfeea1a39b42f/src/priv/supervisor.adoc#L39-L133)

| CSR | 必须? | 理由 |
|---|---|---|
| `sepc` | ✅ | trap 时硬件写入被中断指令地址；每任务必须存（L546-L570） |
| `sstatus` | ✅ | `SPP`（返回特权级）、`SIE`/`SPIE`（中断状态）必须恢复（L104-L121） |
| `stvec` | ❌ | 一次性设置，不被 trap 破坏 |
| `scause` / `stval` | ❌ | 瞬态诊断值，handler 消费后不需要 |
| `satp` | M1 跳过 | 单镜像恒定地址空间；引入 U-mode/多地址空间时按 rCore 补 `kernel_satp` |
| `sscratch` | 协议非字段 | trap 入口 asm 的暂存约定，值由汇编逻辑决定 |

**`sstatus` 与 xv6 的差异**：xv6 不存 sstatus —— 它永远从用户态 trap 返回用户态，返回前
屏蔽 `SPP` 即可（prepare_return）。KaleidOS 有 S-mode 任务，必须按 rCore 模型
忠实保存/恢复 sstatus。

## 4. 两套 context 模型（rCore / xv6 均如此划分）

| 模型 | 保存什么 | 何时用 | 参考 |
|---|---|---|---|
| **Trap context** | x1–x31 + sstatus + sepc（+ 未来 satp） | 抢占/异常（任意指令点） | rCore `TrapContext` |
| **Cooperative context** | 仅 ra + sp + s0–s11 | 调度点显式 yield（函数调用语义） | xv6 `struct context`、rCore `TaskContext` |

无抢占的纯协同调度可用 slim 版；一旦 timer 抢占引入，必须用完整 trap context。
教育内核建议：**一开始就用完整版本**，避免后续重写。

## 5. 参考实现布局

### rCore-Tutorial-v3 `TrapContext`（trap 路径，完整保存）

出处：[os/src/trap/context.rs#L1-L38](https://github.com/rcore-os/rCore-Tutorial-v3/blob/c91bd3752b53ff48555aef4e3c7b8d5ddc8ee6e1/os/src/trap/context.rs#L1-L38)

```rust
#[repr(C)]
pub struct TrapContext {
    pub x: [usize; 32],        // x0..x31
    pub sstatus: Sstatus,
    pub sepc: usize,
    pub kernel_satp: usize,    // kernel page table (U-mode 返回用)
    pub kernel_sp: usize,      // kernel stack (trap handler 用)
    pub trap_handler: usize,   // trap handler 入口
}
```

rCore 的 trap.S 保存序列：跳过 x2（sp 由 `sscratch` 特殊处理）、跳过 x4（tp，应用不用），
其余 x1/x3/x5–x31 直接存，最后 sstatus/sepc 写入偏移 32/33 槽位。

### xv6-riscv `struct context`（协同切换，slim）

出处：[kernel/proc.h#L2-L17](https://github.com/mit-pdos/xv6-riscv/blob/35b088427ef37611c38afdeed5a52a278cae38f9/kernel/proc.h#L2-L17)

```c
struct context {
  uint64 ra;   // 恢复地址
  uint64 sp;
  uint64 s0; ... uint64 s11;   // 12 个 callee-saved
};
// 共 14 字段 = 112 字节，配合 swtch.S 使用
```

### xv6-riscv `struct trapframe`（trap 路径）

出处：[kernel/proc.h#L38-L80](https://github.com/mit-pdos/xv6-riscv/blob/35b088427ef37611c38afdeed5a52a278cae38f9/kernel/proc.h#L38-L80)

```c
struct trapframe {
  // 0   kernel_satp / 8  kernel_sp / 16 kernel_trap / 24 epc / 32 kernel_hartid
  // 40  ra / 48 sp / 56 gp / 64 tp / 72-88 t0-t2 / 96-104 s0-s1 / 112-168 a0-a7
  // 176-248 s2-s11 / 256-280 t3-t6
};
```

注意：xv6 的 trapframe 存全部 GPR（含 tp），但**不存 sstatus**；epc 由 C 代码
`p->trapframe->epc = r_sepc()` 写入（usertrap 里）。

## 6. FPU（RV64GC）

- RV64GC 含 F/D 扩展，浮点寄存器 f0–f31 + `fcsr`（共 33×8+4 字节）。
- psABI FP 约定：fs0–fs11 为 callee-saved（[riscv-cc.adoc#L77-L88](https://github.com/riscv-non-isa/riscv-elf-psabi-doc/blob/01017c343cd6d89ed4d1d568b1c75fac79d2a689/riscv-cc.adoc#L77-L88)）。
- rCore-Tutorial-v3 与 xv6-riscv 的 context 均**不含 FPU 寄存器**（均跳过）。
- KaleidOS M1（S-mode 无浮点任务）：跳过。将来启用浮点任务时：
  惰性方案 `sstatus.FS=Off`（首次浮点指令触发 trap 再保存）或立即保存 264+4 字节。

## 7. KaleidOS 推荐结构（M1）

```rust
/// 完整 trap context（rCore 模型）。M1 单镜像无 U-mode，satp 恒定可略；
/// 引入 U-mode/多地址空间时按 rCore 补 kernel_satp/kernel_sp/trap_handler。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RiscvContext {
    x: [usize; 32],     // x0 占位恒 0；布局必须与 trap 入口 asm 偏移一致
    sstatus: usize,     // SPP/SIE/SPIE（+未来 FS/VS）
    sepc: usize,        // 恢复 PC（硬件 trap 时写入）
}
```

大小 272 字节/任务（slim 版 112B；+160B 换抢占天然支持，M1 正确性优先）。

**布局对齐警告**：`x[32]` 不要无脑按顺序存 —— trap 入口时 `sp` 正从用户栈切到内核栈。
rCore 用 `sscratch` 保存用户 sp：`csrrw sp, sscratch, sp`，结束时把 sp 写回 x2 槽位
（跳过 x4/tp 同理）。**汇编偏移约定必须与 struct 布局完全一致**；
不一致 = 内存损坏、静默失败，调试极难。
