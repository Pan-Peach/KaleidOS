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
extern crate alloc;

#[cfg(test)]
extern crate std;

pub mod component;
pub mod handle;
pub mod inspector;
pub mod irq;
pub mod machine;
pub mod memory;
pub mod monitor;
pub mod object;
#[macro_use]
pub mod print;
pub mod task;
pub mod timer;
pub mod trace;

// 裸机目标才接管全局分配器；host test（std 环境）用 std 默认分配器。
#[cfg(all(not(test), target_os = "none", target_arch = "riscv64"))]
#[global_allocator]
static ALLOCATOR: memory::KernelAllocator = memory::KernelAllocator;

/// Core 初始化入口：消费 bootstrap 发现的 MachineInfo（提案），校验后提交资源真相。
/// `reserved` 是需保留的区间（bootstrap 提供：ELF image range）。
/// 流程：sanity 校验 → 帧区域初始化（frame_start 由 bootstrap 传对齐后的镜像末尾）。
pub fn init(
    info: &machine::MachineInfo,
    reserved: &[machine::MemoryRegion],
) -> Result<(), &'static str> {
    if info.mem_count == 0 {
        return Err("no memory regions");
    }
    if info.cpu_count == 0 {
        return Err("no cpu info");
    }

    // 帧区域：reserved[0] 的末尾（对齐帧）→ 第一个内存 region 的末尾。
    // 前提：BSS 已在 bootstrap 启动汇编里清零（本轮迁移，见 entry.S）；
    // core 不再负责 BSS 清零（那是启动路径职责）。
    let reserved_end = reserved
        .last()
        .map_or(info.memory_regions[0].base, |r| r.base + r.size);
    let frame_start = memory::align_up_frame(reserved_end);
    let frame_end = info.memory_regions[0].base + info.memory_regions[0].size;

    memory::init(frame_start, frame_end)?;

    // 帧真相验证：可分配一帧并释放（自证 allocator 可用）。
    let probe = memory::alloc_frame().map_err(|_| "alloc probe failed")?;
    memory::free_frame(probe).map_err(|_| "free probe failed")?;

    task::init();
    component::registry::init();
    log!("core", "init OK");
    monitor::mount(info);
    Ok(())
}
