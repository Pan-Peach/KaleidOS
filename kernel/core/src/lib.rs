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

/// 帧池容量：4 GiB 物理内存 ÷ 4 KiB 帧 = 1,048,576 帧。
/// Core 持有帧真相的全量数组（BSS 区，~8 MiB）；切片式：FrameDatabase 只借用它。
/// boot 阶段单 hart、无并发，`static mut` 安全；多核唤醒后需改用同步容器。
const MAX_FRAMES: usize = 1 << 20;
static mut FRAME_POOL: [memory::FrameMeta; MAX_FRAMES] = [memory::FrameMeta::new(memory::FrameState::Free); MAX_FRAMES];

/// 借用帧池（boot 单线程，无 race；`static mut` 用 `addr_of_mut!` 规避 `static_mut_refs`）。
fn frame_pool() -> &'static mut [memory::FrameMeta] {
    unsafe { &mut *core::ptr::addr_of_mut!(FRAME_POOL) }
}

/// Core 初始化入口：消费 bootstrap 发现的 MachineInfo（提案），校验后提交资源真相
/// （Resource Truth）。`reserved` 是需保留的区间（由 bootstrap 提供：ELF image range 等）。
/// 流程：sanity 校验 → `memory::init`（帧真相建表 + reserved 标记）→ 提交。
pub fn init(info: &machine::MachineInfo, reserved: &[machine::MemoryRegion]) -> Result<(), &'static str> {
    if info.mem_count == 0 {
        return Err("no memory regions");
    }
    if info.cpu_count == 0 {
        return Err("no cpu info");
    }
    memory::init(&info.memory_regions[..info.mem_count], reserved, frame_pool())?;
    Ok(())
}
