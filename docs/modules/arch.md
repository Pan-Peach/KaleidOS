# arch（os/arch/）

> `os/arch` 是 **ISA / firmware backend crate**：定义 backend contract（trait），并由 cfg 选择 RISC-V 或 host `Fake` 实现。
> 它只提供**机制**，不拥有策略、不拥有启动页表布局、不拥有地址空间生命周期。

## owns 什么真相

`os/arch` 不持有 OS 资源真相；它持有的是**与指令集 / 固件相关的能力契约与具体机制**：

- 后端 trait（定义在 `os/arch/src/lib.rs`）：

| trait | 契约方法 | Riscv 实现 | Fake 实现 |
|---|---|---|---|
| `CpuArch` | `Context`、`IrqFlags`、`context_switch`、`new_context`、`init`、`disable_irq`、`restore_irq`、`wait_for_interrupt` | `riscv/cpu.rs` | `fake/mod.rs` |
| `Timer` | `now`、`set_deadline`、`cancel_deadline`、`register_timer_handler`、`enable_timer_interrupt` | `riscv/cpu.rs` | `fake/mod.rs` |
| `InterruptController` | `configure`、`enable`、`disable`、`claim`、`complete`、`register_external_handler`、`enable_external_interrupt` | `riscv/plic.rs` | `fake/mod.rs` |
| `Console` | `write_byte`、`getc` | `riscv/cpu.rs` | `fake/mod.rs` |
| `SystemReset` | `system_reset(ResetType) -> !` | `riscv/cpu.rs` | `fake/mod.rs` |

- 通用地址方案原语：`HIGH_HALF_OFFSET`、`physical_address_of`（RV64 高半区 → 物理；否则恒等）。
- 架构中立 VM contract（`os/arch/src/vm.rs`）：`AddressSpaceBackend`（`const GRANULE`、`map` / `unmap` / `translate` / `activate`）、`VirtualRange` / `PhysicalRange` / `MappingPermission` / `PageAlloc`。
- 组件传输 / 对象 ABI 机制：`ComponentStore` / `StoreEntry`（`store.rs`）、`RelocationBackend` / `Relocation` / `WordSize`（`component.rs`）。
- 具体 RISC-V 机制：trap 入口与分发、context switch、Sv39 / Sv32 页表编码与 walk、NoMMU identity backend、SBI 边界、PLIC、`RiscvRelocator`。

## 暴露什么机制

- 类型别名（按 cfg 选定具体实现）：`CpuImpl`、`ConsoleImpl`、`ResetImpl`、`TimerImpl`、`InterruptImpl`、`ContextImpl`、`AddressSpaceImpl`、`ComponentRelocationImpl`。
- 关键符号（`os/arch/src/`）：`CpuArch` / `Timer` / `InterruptController` / `Console` / `SystemReset`（`lib.rs`）、`AddressSpaceBackend`（`vm.rs`）、`ComponentStore`（`store.rs`）、`RelocationBackend`（`component.rs`）、`RiscvRelocator`（`riscv/elf.rs`）、`Sv39PageTable` / `Sv32PageTable` / `Sv39AddressSpace` / `Sv32AddressSpace`（`riscv/mmu/`）、`NoMmuAddressSpace`（`nommu.rs`）、`activate` / `flush_tlb`（`riscv/mmu/mod.rs`）、`trap_handler`（`riscv/trap/supervisor.rs`）。

## 明确不做

- **不拥有 boot / kernel 页表策略**：identity + high-half 双映射、段权限、临时 root、`enter_high_half` 全在 boot crate 的 `vm/`；arch **不认识** `KERNEL_VMA` / `.text` / `.initpkg` / bootstrap hand-off。
- **不拥有地址空间生命周期**：`activate` 只碰寄存器；mapping ledger / 所有权在 Core（`memory::address_space`）。
- **不解析 ELF 文件格式**：ELF 解析在 Core（`component/elf.rs`）；arch 只拥有自己的对象 ABI、重定位种类、指令 patch、链接地址归一化。
- **不拥有 SBI 语义身份**：SBI 是 firmware ABI，不是 ISA primitive，隔离在 `riscv/firmware.rs`。
- **不拥有设备所有权 / IRQ route / DMA 记账**：那些在 Core。
- NoMMU 不伪装成 Sv32：无页表、无 `satp`，`GRANULE = 1`，Core 的对齐校验自动退化为 no-op。

## cfg 选择（riscv vs fake）

两条独立轴：

1. **target triple**：`riscv32` / `riscv64` → `Riscv` + Sv39/Sv32；否则 → `fake::Fake`（host 测试默认），`pub mod fake` 只在非 RISC-V target 编译。
2. **Kconfig → Cargo features**：`supervisor` / `machine` 二选一、`vm-mmu` / `vm-nommu` 二选一，由 `compile_error!` 强制；映射集中在 `scripts/kconfig/genmk.py`（`PRIV_MAP` / `VM_MAP` / `ARCH_MAP`），`.config` 是唯一真相。
   - `AddressSpaceImpl`：NoMMU → `NoMmuAddressSpace`；MMU + riscv64 → `Sv39AddressSpace`；MMU + riscv32 → `Sv32AddressSpace`。
   - `ComponentRelocationImpl = RiscvRelocator` 在**所有** target 上，因为重定位是纯编码逻辑，host 测试直接驱动生产实现。

## 代码在哪

| 路径 | 内容 |
|---|---|
| `os/arch/src/lib.rs` | 后端 trait、`ResetType`、cfg 别名、`HIGH_HALF_OFFSET` / `physical_address_of` |
| `os/arch/src/vm.rs` | `AddressSpaceBackend` 与范围 / 权限类型 |
| `os/arch/src/component.rs` / `store.rs` | 重定位契约 / 组件 store 契约 |
| `os/arch/src/nommu.rs` | NoMMU identity backend |
| `os/arch/src/fake/mod.rs` / `fake/store.rs` | host `Fake` backend / `FakeStore` |
| `os/arch/src/riscv/{mod,cpu,console,firmware,plic,elf}.rs` | RISC-V family 机制 |
| `os/arch/src/riscv/context/{mod,switch32.S,switch64.S}` | 上下文切换 |
| `os/arch/src/riscv/trap/{mod,supervisor,machine}.rs` + `trap*.S` | trap 入口 / 分发 / S-mode handler |
| `os/arch/src/riscv/mmu/{mod,address_space,sv32,sv39,test_pool}.rs` | Sv32 / Sv39 页表机制 |
| `os/arch/Kconfig` | `ARCH_*` / `PRIVILEGE_*` / `VM_*` choice |
