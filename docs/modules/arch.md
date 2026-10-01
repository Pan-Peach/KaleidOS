# arch（os/arch/）

> `os/arch` 是 **ISA / firmware backend crate**：定义 backend contract（trait），并由 cfg 选择 RISC-V / x86_64 / AArch64 / LoongArch64 / host `Fake` 实现。
> 它只提供**机制**，不拥有策略、不拥有启动页表布局、不拥有地址空间生命周期。

## owns 什么真相

`os/arch` 不持有 OS 资源真相；它持有的是**与指令集 / 固件相关的能力契约与具体机制**：

- **CPU 身份类型**（`os/arch/src/cpu.rs`，Core `re-export` 为 `core::machine::CpuId` / `HardwareCpuId`）：
  - `CpuId`：**逻辑**身份，稠密、从 0 连续，由 Core 从 discovery 赋号；用作 per-CPU 下标。
  - `HardwareCpuId`：**硬件**身份（hartid / APIC ID / MPIDR / CPUID），稀疏、非数组下标。
  - `LocalInterruptHandler = fn(CpuId)`：timer / IPI 回调的**唯一**注册形状。
  - `ExternalIrqHandler = fn(CpuId, u32)`：外部中断回调形状（逻辑 `CpuId` + 逻辑 IRQ 号）；后端拥有 ack/EOI 与源映射，Core 只收逻辑号。
- 后端 trait（`os/arch/src/lib.rs`）：

| trait | 契约方法 | Riscv 实现 | Fake 实现 |
|---|---|---|---|
| `CpuArch` | `Context`、`IrqFlags`、`context_switch`、`new_context`、`init_cpu`、`enable_irq`、`disable_irq`/`restore_irq`、`wait_for_interrupt`（裸 idle 提示）、`unsafe atomic_idle`（检查-睡眠原子化）、`current_cpu`/`per_cpu_base`/`install_per_cpu_base` | `riscv/cpu.rs` | `fake/mod.rs` |
| `Timer` | `init_cpu`（本地、disarmed、masked，`Result<(), TimerError>`）、`now`、`set_deadline`（`Result`；`Ok` = 真实 deadline 已编程）、`cancel_deadline`、`register_timer_handler(LocalInterruptHandler)`、`enable_timer_interrupt`（只解投递路径、`Result`；`Ok` = 回调真的会被调用） | `riscv/cpu.rs` | `fake/mod.rs` |
| `InterruptController` | `Config`、`unsafe configure`、`init_cpu`、`enable`/`disable`、`register_external_handler(ExternalIrqHandler)`、`enable_external_interrupt`；**ack/EOI 与源映射是后端私有**（PLIC claim/complete、claim 令牌不进 trait），Core 只按逻辑 IRQ 号路由 | `riscv/plic.rs`（`PlicConfig`） | `fake/mod.rs` |
| `Smp` | `BootConfig`、`unsafe prepare`、`unsafe start_cpu`、`init_ipi_cpu`（本地 IPI 接收，保持 masked）、`register_ipi_handler`、`enable_ipi_interrupt`、`send_ipi`、`send_ipi_mask` | `riscv/smp.rs`（骨架） | `fake/mod.rs` |
| `Console` | `write_byte`、`getc` | `riscv/console.rs` | `fake/mod.rs` |
| `SystemReset` | `system_reset(ResetType) -> !` | `riscv/cpu.rs` | `fake/mod.rs` |

> **timer 的诚实性契约**：`TimerError { Unsupported, DeliveryUnavailable, HardwareFailure }`。
> `init_cpu` / `enable_timer_interrupt` 返回 `Ok` 必须意味着投递链路端到端可用；
> `set_deadline` 返回 `Ok` 必须意味着一个**真实 deadline** 被编程（不是"某个无关的周期 tick 正好在跑"）。
> 能力存在但路由 / CPU interface / trap 分发缺失时必须返回 `DeliveryUnavailable`，绝不假装成功。
> 当前映射：RISC-V（SBI TIME）可用；x86_64 只有可读 TSC、无 deadline 投递 → `Unsupported` / `DeliveryUnavailable`；
> AArch64 架构 timer 可编程但 GICv3 PPI 路由未接通 → `DeliveryUnavailable`；LoongArch64 骨架 → `Unsupported`。
>
> **`atomic_idle` 契约**（替代旧 Core 里的 "masked WFI" 汇编）：调用前必须已由 `disable_irq()` 关中断，
> 并在关中断状态下 arm 好唤醒源；`flags` 为"中断已启用"时不得在 enable→sleep 窗口丢 pending 中断；
> `flags` 为"已关闭"时恢复并立即返回、不睡眠；实现用各 ISA 的原子序列（x86 `sti; hlt; cli`、
> RISC-V 全局中断关闭的 masked-WFI、AArch64 pending-IRQ 检查 + WFI），**不是** `restore_irq + wait_for_interrupt`。
> `wait_for_interrupt` 退回为**裸 idle 提示**：可能 spurious / 永不返回、不 arm timer、不改中断状态。

> **`tp`（x4）只是架构 / 任务执行状态（线程指针）**，不是组件运行时指针，也没有 per-instance runtime slot：`RiscvContext.tp` 与 `switch32.S` / `switch64.S` 在任务切换、`TrapFrame.x[4]` 在 trap 时由 Core 透明保存 / 恢复；全新执行上下文起点为 `tp == 0`；跨 AS trampoline 对全新同步 Isolated 入口**显式清零 `tp`**（不继承 caller 的 `tp`）。TLS / 线程指针语义属未来 task/thread/libc runtime，**不属于 Component 模型**；Core 的 principal / authority 来自 `RequestContext::ambient()`（containment escape-guard 链），与 `tp` 无关。

- **不做通用地址方案原语**：公共 HAL 不承诺任何固定 VA↔PA 偏移（无 `HIGH_HALF_OFFSET` / 无 `physical_address_of`）。运行期 VA→PA 一律走 `AddressSpaceBackend::translate`（由映射所有者裁决）；RISC-V 高半区换算只出现在 boot 本地（`vm/bootstrap.rs`）与该 ISA 的**组件导入 / callable 绑定**（`riscv/elf.rs`，不是 VA→PA 查询）内部。
- 架构中立 VM contract（`os/arch/src/vm.rs`）：`AddressSpaceBackend`（`GRANULE` / `PRIVATE_ADDRESS_SPACE` / `map` / `unmap` / `translate` / `activate` / `prepare_activation`）。
- 组件传输 / 对象 ABI 机制：`ComponentStore` / `StoreEntry`（`store.rs`）、`RelocationBackend` / `Relocation` / `WordSize`（`component.rs`）。
- 具体 ISA 机制：RISC-V trap / context switch / Sv39·Sv32 页表 / SBI / PLIC / `RiscvRelocator` / 跨 AS trampoline；新 ISA 为同形骨架（见下）。

## 暴露什么机制

- 类型别名（按 cfg 选定具体实现）：`CpuImpl`、`ConsoleImpl`、`ResetImpl`、`TimerImpl`、`InterruptImpl`、`SmpImpl`、`ContextImpl`、`AddressSpaceImpl`、`ComponentRelocationImpl`。
- 关键符号（`os/arch/src/`）：`CpuArch` / `Timer` / `InterruptController` / `Smp` / `Console` / `SystemReset`（`lib.rs`）、`CpuId` / `HardwareCpuId` / `LocalInterruptHandler` / `ExternalIrqHandler`（`cpu.rs`）、`Smp` 契约类型（`smp.rs`）、`AddressSpaceBackend`（`vm.rs`）、`RiscvRelocator`（`riscv/elf.rs`）、`Sv39PageTable` / `Sv32PageTable` / `Sv39AddressSpace` / `Sv32AddressSpace`（`riscv/mmu/`）、`PlicConfig`（`riscv/plic.rs`）、`trap_handler`（`riscv/trap/supervisor.rs`）、`trampoline_enter` / `trampoline_return`（`riscv/trampoline/`）。

## SMP 状态（不是构建开关）

**SMP 不是构建开关**：没有 `smp` Cargo feature，boot 的 SMP 模块按 `riscv64 + vm-mmu` 无条件编译（`.config` 里的 `CONFIG_SMP` 是历史残留，任何 Kconfig 都未定义它）。UP = 只有 CPU0 的 SMP。

**不变式：逻辑 CPU0 = boot hart。** OpenSBI 用**抽签**选 boot hart，它不一定是 DTB 里第一个 CPU；boot 在 discovery 后**归一化**（`main64.rs` / `main32.rs`），使 BSP 恒为逻辑 0。PLIC 外部固定路由、`trap_stack_range()`、per-CPU 表都依赖这条不变式（不归一化时 `smp-percpu` / `external-irq` 实测 flaky）。

- **已实现（RISC-V / Fake）**：`CpuId`/`HardwareCpuId` 分离；回调分两类——`LocalInterruptHandler = fn(CpuId)`（timer / IPI）与 `ExternalIrqHandler = fn(CpuId, u32)`（外部中断，逻辑 IRQ 号）；`InterruptController` 的 `Config` + `init_cpu`（`PlicConfig` 携带**逻辑 CPU → PLIC context 映射表** + 固定路由 CPU + source 上界；enable bank 读改写持锁 + irq-save）；**后端拥有 ack/EOI 与源映射**（PLIC claim/complete、claim 令牌 backend-private；`configure` 装入后端分发器，循环 `claim → 逻辑号 → Core 回调 → complete`），Core 只收逻辑 IRQ 号并按 route 表投递；`CpuArch::init_cpu`/`enable_irq`/`current_cpu`/`per_cpu_base`/`install_per_cpu_base`/`atomic_idle`（RISC-V：全局中断关闭下的 masked-WFI）；`Timer::init_cpu` + 诚实 readiness（`Ok` = 投递可用）；`Smp::init_ipi_cpu`/`register_ipi_handler`/`enable_ipi_interrupt`/`send_ipi*`；中断使能生命周期显式化（本地 `init_cpu`/解源 与全局 `enable_irq` 分离）。Fake 后端提供确定性故障注入（init / delivery / arm 失败）与有序 `TimerEvent` 日志，供 Core 测试断言检查-睡眠协议；`deliver_external_for_test` 直接驱动 Core `on_irq` 生产路径。
- **Core 侧（`os/core/src/smp/`，已落地并接线）**：`CpuMask`/`PerCpu`/`CpuBootState`/`BootGate` + 记录表 `CpuRegistry`；`init`（BSP 侧：校验拓扑、注册 Core IPI 回调、BSP Online）；`cpu_state`/`online_cpus`/`request_start`/`mark_ready`/`cpu_identity_ok`；`ipi::notify`/`ipi_interrupt`/`drain_pending`（pending 位 → 延迟重调度标志）；`secondary_entry`（AP 入场：绑入口记录 → per-CPU sched/timer/irq → 身份验证 → Ready → BootGate → Online → Core 空闲循环）；`release_secondaries`/`wait_until_online`；`timer::STATE` 已 per-CPU。`core::init` 调 `smp::init`；boot 的 `secondary_main` 现为指向 Core 的**薄 trampoline**，`start_secondaries` 驱动 `request_start` + `release_secondaries`。**AP 已进入 Core 并 Online**（`smp-boot`/`smp-ipi`/`smp-percpu` 经 Core 状态断言）。
- **仍是 `todo!()`（人类，关键并发路径）**：arch `Smp::prepare`/`start_cpu`（启动 seam，可 still boot 驱动）；`containment` 的进程级 `static mut` → per-CPU（AP 跑组件任务的前提）；跨 CPU **任务放置 / 迁移**的离场上下文交接（Oracle 最高风险：`sched.rs` 先发布离场状态、后保存上下文）；抢占（C5）。
- **协调项**：外部 IRQ 仍固定路由 BSP；AP 目前只做 Core 空闲循环（不跑组件任务），因此 `containment::init_cpu` 仍滞留 `todo!()`（AP 不触发它）。详见 `.omo/plans/smp-production-integration.md`。
- **测试入口**：`make test-arch`（rv64+rv32，默认）；`make test-arch-smp-rv64`（`smp-boot`/`smp-ipi`/`smp-percpu`，已确定性通过）；`make test-arch-{x86_64,aarch64,loongarch64}` / `test-arch-new`（opt-in 新 ISA，boot 未实现前会失败）。

## 新 ISA 骨架（x86_64 / aarch64 / loongarch64）

每个新 ISA 与 riscv **同形**（`<isa>/mod.rs` + `encoding.rs` + `elf.rs` + `console.rs` + `cpu.rs` + `smp.rs` + `trap/` + `context/` + `mmu/`），**实现体一律 `todo!()`**；纯编码模块（`encoding.rs`）可在 host `test` 下编译与单测（当前以 `#[ignore]` 保留，实现后去掉）。

- `encoding.rs`：APIC/ICR（x86_64）、MPIDR/PSCI/SGI（aarch64）、CSR/IOCSR mailbox/IPI（loongarch64）等纯格式。
- `elf.rs`：各 ISA 的 `RelocationBackend`，`ELF_MACHINE` = 62 / 183 / 258；未实现时 `apply` 返回 `RelocationError::Unsupported`，**绝不误用 RISC-V 语义**。
- `AddressSpaceImpl`：新 ISA 未实现前用显式占位 `stub_vm::StubAddressSpace`（`PRIVATE_ADDRESS_SPACE = false`），不假装成 Sv39。
- **timer 诚实性（新 ISA）**：x86_64 只有可读 TSC（`now` 可用），没有 deadline 投递，且**不再**在 IDT 安装时启动周期 PIT —— `init_cpu`/`set_deadline` 返回 `Unsupported`、`enable_timer_interrupt` 返回 `DeliveryUnavailable`，控制台靠 Core 轮询继续工作；AArch64 的 `init_cpu` / `set_deadline` 真实可用（`cancel_deadline` 后再次 `set_deadline` 会显式重开 `CNTV_CTL.ENABLE`），但 GICv3 PPI 路由未接通 → `enable_timer_interrupt` 返回 `DeliveryUnavailable`；LoongArch64 骨架一律 `Unsupported` / `DeliveryUnavailable`。
- boot：`os/boot/{x86_64,aarch64,loongarch64}` 骨架（`_start` `todo!()`）；`.kcomp` 组件目前仍是 RISC-V 重定位专用，所以新 ISA ArchTest 用 **Core-only** 镜像。

## 明确不做

- **不拥有 boot / kernel 页表策略**：identity + high-half 双映射、段权限、临时 root 全在 boot crate 的 `vm/`。
- **不拥有地址空间生命周期**：`activate` 只碰寄存器；mapping ledger / 所有权在 Core。
- **不解析 ELF 文件格式**：ELF 解析在 Core；arch 只拥有自己的对象 ABI 与重定位。
- **不拥有 SBI 语义身份**：SBI 隔离在 `riscv/firmware.rs`。
- **不拥有设备所有权 / IRQ route / DMA 记账**：那些在 Core。
- NoMMU 不伪装成 Sv32：无页表、无 `satp`，`GRANULE = 1`。
- **不做**调度策略、负载均衡、跨 CPU RPC、远端 timer 编程、通用 `ack_ipi`（见 `smp.rs` 边界）。

## cfg 选择（ISA 后端）

两条独立轴：

1. **target triple + `target_os`**：
   - `riscv32`/`riscv64` → `Riscv`；
   - `all(target_arch = "x86_64", target_os = "none")` → `X86_64`，aarch64 / loongarch64 同理；
   - **host（`not(target_os = "none")`）→ `Fake`**（host 上的 `x86_64` 必须落到 `Fake`，不能按 `target_arch` 单独判）；
   - 未支持的裸机目标 → 显式 `compile_error!`。
2. **Kconfig → Cargo features**：`supervisor`/`machine` 二选一、`vm-mmu`/`vm-nommu` 二选一、`smp`（CONFIG_SMP）；映射集中在 `scripts/kconfig/genmk.py`（`ARCH_MAP` / `PRIV_MAP` / `VM_MAP`），`.config` 是唯一真相。`KCFG_BOOT_DIR` 也由 `ARCH_MAP` 给出（Makefile 不再写死 `os/boot/riscv`）。

## 代码在哪

| 路径 | 内容 |
|---|---|
| `os/arch/src/lib.rs` | 后端 trait、`ResetType`、cfg 别名 |
| `os/arch/src/cpu.rs` | `CpuId` / `HardwareCpuId` / `LocalInterruptHandler` / `ExternalIrqHandler` |
| `os/arch/src/smp.rs` | `Smp` trait、`SecondaryBoot`、`InitError`/`CpuStartError`/`IpiError` |
| `os/arch/src/vm.rs` | `AddressSpaceBackend` 与范围 / 权限类型 |
| `os/arch/src/component.rs` / `store.rs` | 重定位契约 / 组件 store 契约 |
| `os/arch/src/stub_vm.rs` | 新 ISA 的显式占位地址空间 |
| `os/arch/src/nommu.rs` / `fake/` | NoMMU identity backend / host `Fake` backend |
| `os/arch/src/riscv/{mod,cpu,console,firmware,plic,elf,smp}.rs` | RISC-V family 机制 |
| `os/arch/src/riscv/{context,trap,mmu,trampoline}/` | 上下文 / trap / 页表 / 跨 AS 原语 |
| `os/arch/src/{x86_64,aarch64,loongarch64}/` | 新 ISA 同形骨架（`todo!()`） |
