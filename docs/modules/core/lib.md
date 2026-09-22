# lib（os/core/src/lib.rs）

> Resource Core 的 **library 根**：声明公共模块、定义 `init()` 装配顺序、安装裸机全局分配器、导出 `printk!` / `log!`。
> 它本身不持有资源真相——真相分散在各模块，`init()` 负责按依赖顺序把它们装起来。

## owns 什么真相

无。它是**依赖契约 + 初始化编排**：只依赖 `os/arch` 的稳定 backend contract，消费已归一化的 `machine::MachineInfo`；**不认识 FDT / ACPI / QEMU / boot**。

## 暴露什么机制

- `pub fn init(info: &machine::MachineInfo, reserved: &[machine::MemoryRegion]) -> Result<(), &'static str>`
  校验 `MachineInfo` → 求帧区域 → `memory::init` + 分配/释放探测 → 依次 `task::init()`、`sched::init()`、`component::containment::init()`、`timer::init()`/`init_preempt(...)`、`component::image::init()`、`component::registry::init()`、`component::interface::init()`、`resource::init()`、`irq::init()`，最后 `monitor::mount(info)`（`MachineInfo` 在此提交）。
- 公共模块：`bench`、`component`、`errno`、`generated`、`inspector`、`irq`、`machine`、`memory`、`monitor`、`object`、`print`（`#[macro_use]`）、`resource`、`sched`、`task`、`timer`、`trace`。
- 宏：`printk!` / `log!`（`print.rs` 中 `#[macro_export]`）。
- 裸机全局分配器：`#[global_allocator] static ALLOCATOR: memory::KernelAllocator`，仅在 `not(test)` + `target_os = "none"` + riscv32/64 下安装。
- 仅供测试的私有模块：`build_config`（`#[cfg(test)]`）。

## 明确不做

- 不持有 ISA / 机器发现 / 组件 / bootstrap 逻辑。
- **不做 BSS 清零**——那是启动路径（boot `entry*.S`）的职责。

## 代码在哪

| 位置 | 内容 |
|---|---|
| `os/core/src/lib.rs` | 模块声明、`init()` 编排、全局分配器、宏导出 |
| `os/core/src/build_config.rs` | test-only 的 `TRACE_CAPACITY` 传输契约（见 [`build_config.md`](build_config.md)） |
