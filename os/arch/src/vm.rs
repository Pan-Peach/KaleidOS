//! Architecture-neutral VM backend contract.
//!
//! Core 的 `KernelAddressSpace`（os/core/src/memory/address_space.rs）直接消费
//! 这里的 `AddressSpaceBackend` / `VirtualRange` / `PhysicalRange` /
//! `MappingPermission`；`PageAlloc` 是 core buddy heap 给页表 backend 的窄回调。

use bitflags::bitflags;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VirtualRange {
    pub base: usize,
    pub size: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PhysicalRange {
    pub base: usize,
    pub size: usize,
}

// 逻辑权限：正交的位集合（arch 无关）。默认为空 = 无权限。
// 到具体 arch 的编码（如 Sv39/Sv32 的 R/W/X/U）由各后端在 `From` 里翻译。
bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
    pub struct MappingPermission: u8 {
        const READ    = 1 << 0;
        const WRITE   = 1 << 1;
        const EXECUTE = 1 << 2;
        const USER    = 1 << 3;
    }
}

/// 一个必须**在 Core AS 与目标实例 AS 中以同一 VA → 同一 PA** 出现的机制页
/// （assembly gateway：代码页 + 入口 scratch 页）。
///
/// Core 把 arch 给出的这对 range 落成实例 AS 的一条映射；因为 Core AS 里同一
/// VA 已经指到同一 PA，切换 `satp` 前后 PC / 数据访问都连续，不必在两个 root
/// 里维护不同的 gateway VA。它是**机制页，不是组件资源**：除这些页之外，实例
/// AS 只应包含该实例自己的内存（Core 段 / Core 堆 / 页表 / MMIO 都不进）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DualMappedPage {
    pub virtual_range: VirtualRange,
    pub physical_range: PhysicalRange,
    pub permission: MappingPermission,
}

/// 一次性"给一个已归零的页，返回其物理地址"的钩子。
///
/// 这就是 buddy allocator（`core::memory::alloc_region`）的窄接口：
/// 因为 `os/arch` 不能依赖 `os/core`，Core 在初始化时把这个函数地址塞进
/// 当前 RISC-V 页表 backend。v1 identity 阶段返回的物理地址可直接当虚拟地址解引用。
pub type PageAlloc = fn() -> Result<usize, ()>;

/// Contract `KernelAddressSpace` drives. Methods take raw ranges/permissions;
/// Core validates & commits around the call. The active architecture backend
/// supplies the concrete implementation.
pub trait AddressSpaceBackend {
    /// 该后端要求映射区间满足的对齐/步进粒度（必须是 2 的幂）。
    ///
    /// Core 的地址空间校验用它做对齐检查，**不引用任何分配器常量**：
    /// Sv39/Sv32 = `VM_PAGE_SIZE`（4 KiB）；NoMMU = 1（恒等，无对齐约束，
    /// 校验自动退化为 no-op）。这保证 Core 不偷偷依赖"必须有 MMU 页"。
    const GRANULE: usize;

    /// 该后端是否提供**私有地址空间**能力（独立页表 + 切换机制）。
    ///
    /// **`AddressSpaceBackend` 可用 ≠ 有隔离能力**：NoMMU 恒等翻译同样实现本
    /// trait，但 `VA == PA`、无页表、无 satp——无法承载 Isolated 域，声明
    /// `false`。Core 的部署/装载路径据此**显式拒绝**（绝不把 NoMMU 当私有 AS
    /// 用）。除了"私有 AS 存在"，本常量**不**表达任何安全承诺：S-mode 换页表
    /// 是协作式、非对抗边界（见 `docs/development/roadmap.md` §10.1）。
    const PRIVATE_ADDRESS_SPACE: bool;

    /// 切换汇编所需的原始数据：由 backend 打包，Core **只搬运、不解释**。
    ///
    /// 必须是 `Copy` 且不携带借用：描述符在 Core 锁内准备，之后**不得**再触碰
    /// Core 锁或 Rust 栈（真正的 satp 切换发生在汇编路径上，可能已经换根）。
    type Activation: Copy;

    type Error;

    fn create(alloc: PageAlloc) -> Result<Self, Self::Error>
    where
        Self: Sized;

    fn map(
        &mut self,
        va: VirtualRange,
        pa: PhysicalRange,
        perm: MappingPermission,
    ) -> Result<(), Self::Error>;
    fn unmap(&mut self, va: VirtualRange) -> Result<(), Self::Error>;
    fn translate(&self, va: usize) -> Option<usize>;
    fn activate(&self) -> Result<(), Self::Error>;

    /// 准备切换数据：只读，**不写 satp、不刷 TLB、不改状态**。
    ///
    /// 真正的寄存器写入仍在 `activate()`（切换汇编消费本返回值，见 `gateway`）。
    fn prepare_activation(&self) -> Self::Activation;
}
