//! KaleidOS 内核 —— Resource Core（真相：存在性/状态/所有权/生命周期）。
//!
//! 依赖方向：本 crate（Resource Core **library**）不依赖具体 Arch / Discovery backend /
//! 组件 / bootstrap；Core 只消费归一化的 MachineInfo（`machine::MachineInfo`），
//! 不知道 FDT / ACPI / QEMU / 板子，也不知道加载器是谁。
//! 本 crate 是 host-testable 的 library（`cargo test` 专用）；运行时常与 bootstrap 阶段
//! 一起链接成 `kaleidos.elf`（单镜像，职责分离装载合一，见 `docs/architecture.md` §3）。
//! ISA 层（`kernel/arch/`）与 FDT 解析（`third_party/fdt` 子模块）是独立依赖。
//! 设计契约见 `docs/architecture.md` 与 `docs/core-philosophy.md`。

#![no_std]

#[cfg(test)]
extern crate std;

pub mod component;
pub mod handle;
pub mod inspector;
pub mod irq;
pub mod machine;
pub mod memory;
pub mod object;
pub mod task;
pub mod timer;
pub mod trace;

/// Core 初始化入口：消费 bootstrap 发现的 MachineInfo（提案），校验后提交资源真相
/// （Resource Truth）。M0 只做 sanity 校验；FrameId 资源模型（memory.rs）由人类实现者填充。
pub fn init(info: &machine::MachineInfo<'_>) -> Result<(), &'static str> {
    if info.memory_regions.is_empty() {
        return Err("no memory regions");
    }
    if info.cpu_info.is_empty() {
        return Err("no cpu info");
    }
    // TODO(人类实现者): 校验 region 对齐/重叠 → 建立 FrameId 真相 → 提交
    Ok(())
}
