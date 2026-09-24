//! Runtime address-space vocabulary and ownership skeleton.
//!
//! This module intentionally contains semantic Core state only. The concrete
//! translation representation belongs to an architecture backend.

use crate::component::ComponentId;

// 共享词汇表直接复用 arch::vm（os/core 依赖 os/arch，方向正确）。
// 这里 re-export 一份，让 `address_space::PhysicalRange` 等对 memory/mod.rs 仍可用。
pub use arch::vm::{AddressSpaceBackend, MappingPermission, PhysicalRange, VirtualRange};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AddressSpaceId(u32);

impl AddressSpaceId {
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AddressSpaceHandle {
    id: AddressSpaceId,
    generation: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressSpaceState {
    /// **已准备、未退役**：可以落映射、可以准备激活描述符。
    ///
    /// 它**不**表示"当前已在硬件上激活"——Core 不跟踪"当前 satp 是谁"（本阶段
    /// 没有任何运行期切换）；激活是切换汇编消费描述符的动作，不在本模块记账。
    Ready,
    /// **已退役**：不再接受任何映射 / 解映射 / 激活准备，`translate` 返回
    /// `None`。只进不出（复用 = 新建一个空间）。
    Retired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mapping {
    pub virtual_range: VirtualRange,
    pub physical_range: PhysicalRange,
    pub permission: MappingPermission,
}

/// Core 把已批准的映射翻译成 arch backend 的原始参数形式。
/// 现在 `Mapping` 字段本身就是 arch 类型，所以直接透传即可。
impl Mapping {
    pub(crate) fn backend_parts(self) -> (VirtualRange, PhysicalRange, MappingPermission) {
        (self.virtual_range, self.physical_range, self.permission)
    }
}

/// 一个**已准备、可脱离 Core 锁**的激活描述符（`AddressSpaceBackend::Activation`
/// 的 Core 侧包装：空间身份 + backend 私有的原始切换数据）。
///
/// 用途：未来的切换路径在**持锁期间**取一次描述符，之后即使换页表根、不再触碰
/// Core 的 Rust 互斥量或 Rust 栈，也能用 `token()` 里的原始数据完成汇编切换。
/// 因此它必须是 `Copy`、不携带引用、且只能由 [`KernelAddressSpace::prepare_activation`]
/// 构造——**不是**组件可见的能力（不经任何 `kcore_*` 导出）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreparedActivation<A: Copy> {
    handle: AddressSpaceHandle,
    token: A,
}

impl<A: Copy> PreparedActivation<A> {
    /// 被准备的空间身份（供 Core 记账 / trace；切换汇编不解释它）。
    pub fn handle(self) -> AddressSpaceHandle {
        self.handle
    }

    /// backend 私有的原始切换数据（Core 只搬运，不解释）。
    pub fn token(self) -> A {
        self.token
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    EmptyRange,
    LengthMismatch,
    Unaligned,
    Overlap,
    AddressOverflow,
    /// 传入的虚拟区间不是一条**已记录的整段映射**（精确匹配失败；不触碰后端）。
    NotMapped,
    /// 句柄不存在（未知 id / generation 不匹配）。
    NoSuchSpace,
    /// 目标地址空间已退役（`AddressSpaceState::Retired`）。
    Retired,
    /// 该构建 / profile 没有私有地址空间能力（NoMMU 或后端缺失）——
    /// 不支持的操作显式拒绝，绝不静默降级。
    Unsupported,
    BackendFailed,
}

/// 对齐检查委托给 backend 的 `GRANULE`（不引用分配器常量）。
/// GRANULE 必须是 2 的幂；NoMMU backend 用 1 时这里自动退化为恒真。
fn is_aligned<B: AddressSpaceBackend>(addr: usize) -> bool {
    addr & (B::GRANULE - 1) == 0
}

fn ranges_overlap(a: &VirtualRange, b: &VirtualRange) -> bool {
    a.base < b.base + b.size && b.base < a.base + a.size
}

/// Core-owned logical address space: 资源/权限/生命周期对象。
///
/// 持有真相（state / owner / mapping 列表）与一个不透明的架构后端 `B`。
/// 映射流程：Core `validate` -> backend 写 PTE -> Core `commit` 记录真相。
pub struct KernelAddressSpace<B: AddressSpaceBackend> {
    id: AddressSpaceId,
    generation: u32,
    owner: ComponentId,
    state: AddressSpaceState,
    mappings: alloc::vec::Vec<Mapping>,
    backend: B,
}

impl<B: AddressSpaceBackend> KernelAddressSpace<B> {
    pub fn new(id: AddressSpaceId, generation: u32, owner: ComponentId, backend: B) -> Self {
        Self {
            id,
            generation,
            owner,
            state: AddressSpaceState::Ready,
            mappings: alloc::vec::Vec::new(),
            backend,
        }
    }

    pub fn handle(&self) -> AddressSpaceHandle {
        AddressSpaceHandle {
            id: self.id,
            generation: self.generation,
        }
    }

    /// 只读访问。字段本身是 private，防止外部绕过 `map`/`unmap`/`activate`
    /// 直接改真相或后端。
    pub fn id(&self) -> AddressSpaceId {
        self.id
    }

    pub fn owner(&self) -> ComponentId {
        self.owner
    }

    pub fn state(&self) -> AddressSpaceState {
        self.state
    }

    pub fn mappings(&self) -> &[Mapping] {
        &self.mappings
    }

    /// **精确查询**：只匹配虚拟区间**完全相等**的已记录映射（不做包含 / 部分
    /// 匹配）。release 路径靠它按"当初 acquire 的精确 extent"回找，而不是
    /// "某个 PTE 存在"——后者会在别名映射下猜错。
    pub fn mapping_exact(&self, range: &VirtualRange) -> Option<&Mapping> {
        self.mappings.iter().find(|m| m.virtual_range == *range)
    }

    /// 退役后的所有 mutation（含激活准备）一律拒绝；只读查询降级为 `None`。
    fn ensure_ready(&self) -> Result<(), MapError> {
        match self.state {
            AddressSpaceState::Ready => Ok(()),
            AddressSpaceState::Retired => Err(MapError::Retired),
        }
    }

    /// Core 校验一次映射：非空、等长、页对齐、不重叠。只接受批准后的映射。
    ///
    /// - `virtual_range.size == 0 || physical_range.size == 0` -> `EmptyRange`
    /// - `virtual_range.size != physical_range.size`           -> `LengthMismatch`
    /// - `base` 或 `size` 未页对齐                               -> `Unaligned`
    /// - 与任一已有 mapping 的虚拟区间重叠                       -> `Overlap`
    ///
    /// 物理区重叠不查：同一物理页映射到多个 VA 是合法的（别名映射）。
    fn validate(&self, mapping: &Mapping) -> Result<(), MapError> {
        let vr = mapping.virtual_range;
        let pr = mapping.physical_range;
        if vr.size == 0 || pr.size == 0 {
            return Err(MapError::EmptyRange);
        }
        if vr.size != pr.size {
            return Err(MapError::LengthMismatch);
        }
        if !is_aligned::<B>(vr.base) || !is_aligned::<B>(pr.base) {
            return Err(MapError::Unaligned);
        }
        if !is_aligned::<B>(vr.size) || !is_aligned::<B>(pr.size) {
            return Err(MapError::Unaligned);
        }
        // 先做 checked 溢出，保证后续 `ranges_overlap` 里 `base+size` 不绕回。
        // 否则溢出的区间可能让 backend 一页不写却报告成功，破坏 Core truth。
        vr.base
            .checked_add(vr.size)
            .ok_or(MapError::AddressOverflow)?;
        pr.base
            .checked_add(pr.size)
            .ok_or(MapError::AddressOverflow)?;
        for m in &self.mappings {
            if ranges_overlap(&m.virtual_range, &vr) {
                return Err(MapError::Overlap);
            }
        }
        Ok(())
    }

    /// 映射流程：1) Core 验证  2) 后端写 PTE  3) Core 记录真相。
    ///
    /// 后端写 PTE 失败即整体失败，不 record 到 `mappings`（后端内部负责回滚）。
    /// 已退役的空间先拒绝（`MapError::Retired`），不触碰后端与真相。
    pub fn map(&mut self, mapping: Mapping) -> Result<(), MapError> {
        self.ensure_ready()?;
        self.validate(&mapping)?;
        let (va, pa, perm) = mapping.backend_parts();
        self.backend
            .map(va, pa, perm)
            .map_err(|_| MapError::BackendFailed)?;
        self.commit(mapping);
        Ok(())
    }

    /// 把已批准并落地的映射写入真相列表。
    fn commit(&mut self, mapping: Mapping) {
        self.mappings.push(mapping);
    }

    /// 解除一个**精确的**映射：找到虚拟区间完全相等的记录，让后端清整段映射，
    /// 再从 `mappings` 里移除该记录。v1 不做 partial unmap。
    ///
    /// 若传入的区间不是已记录的整段映射，返回 `NotMapped`，不触碰后端与真相列表。
    /// 已退役的空间返回 `Retired`（同样是"不触碰"）。
    pub fn unmap(&mut self, range: &VirtualRange) -> Result<(), MapError> {
        self.ensure_ready()?;
        let index = self
            .mappings
            .iter()
            .position(|m| m.virtual_range == *range)
            .ok_or(MapError::NotMapped)?;

        let actual_range = self.mappings[index].virtual_range;
        self.backend
            .unmap(actual_range)
            .map_err(|_| MapError::BackendFailed)?;
        self.mappings.remove(index);
        Ok(())
    }

    /// 把虚拟地址翻译成物理地址，委托给后端。
    /// 已退役的空间返回 `None`（不接受任何查询）。
    pub fn translate(&self, va: usize) -> Option<usize> {
        if self.state != AddressSpaceState::Ready {
            return None;
        }
        self.backend.translate(va)
    }

    /// 准备一次切换所需的原始数据（只读，不写 satp、不刷 TLB、不改状态）。
    ///
    /// 描述符 `Copy` 且不携带借用：Core 锁释放后仍可安全使用。
    pub fn prepare_activation(&self) -> Result<PreparedActivation<B::Activation>, MapError> {
        self.ensure_ready()?;
        Ok(PreparedActivation {
            handle: self.handle(),
            token: self.backend.prepare_activation(),
        })
    }

    /// 把本地址空间激活为当前 satp（委托后端写 satp + sfence）。
    ///
    /// 本阶段**没有任何运行期调用方**（切换留给后续 increment）；保留它给
    /// boot 等已激活路径，退役的空间不允许再激活。
    pub fn activate(&self) -> Result<(), MapError> {
        self.ensure_ready()?;
        self.backend.activate().map_err(|_| MapError::BackendFailed)
    }

    /// 标记该地址空间**退役**：此后 map / unmap / prepare_activation / activate
    /// 一律 `MapError::Retired`，translate 返回 `None`。幂等（重复退役是 no-op）。
    ///
    /// 复用 = 新建空间；退役的空间不再有任何"复活"语义。
    pub fn retire(&mut self) {
        self.state = AddressSpaceState::Retired;
    }

    /// 登记一个**已存在**后端的既有映射真相（`AddressSpaceManager::adopt` 用）。
    /// Core 不探测后端，调用方必须给出完整、精确的清单。
    pub(crate) fn adopt_mappings(&mut self, mappings: alloc::vec::Vec<Mapping>) {
        self.mappings = mappings;
    }
}

/// Core authority boundary for create/get/get_mut。
pub struct AddressSpaceManager<B: AddressSpaceBackend> {
    spaces: alloc::vec::Vec<KernelAddressSpace<B>>,
    next_id: u32,
}

impl<B: AddressSpaceBackend> AddressSpaceManager<B> {
    pub const fn empty() -> Self {
        Self {
            spaces: alloc::vec::Vec::new(),
            next_id: 0,
        }
    }

    pub fn create(&mut self, owner: ComponentId, backend: B) -> AddressSpaceHandle {
        self.adopt(owner, backend, alloc::vec::Vec::new())
    }

    /// 接管一个**已存在**的后端（典型：boot 当时已建立并激活的长期 root），连同
    /// 调用方声明的既有映射真相一起登记。
    ///
    /// Core **不探测后端**：`mappings` 必须由调用方给出完整、精确的清单（boot
    /// root 的映射只有 boot 知道）。本 hook 存在但**尚未被 boot 调用**——boot 的
    /// `RuntimeVm` 仍按值持有 backend 并负责后续追加映射，把所有权搬进这里需要
    /// 先重构 boot 的 `vm/runtime.rs`（装段 → 登记 → 之后经 Core 追加映射），
    /// 本轮不做，避免在无 host 测试的 boot 路径上强行改结构。
    pub fn adopt(
        &mut self,
        owner: ComponentId,
        backend: B,
        mappings: alloc::vec::Vec<Mapping>,
    ) -> AddressSpaceHandle {
        let id = AddressSpaceId::from_raw(self.next_id);
        self.next_id += 1;
        let mut space = KernelAddressSpace::new(id, 1, owner, backend);
        space.adopt_mappings(mappings);
        self.spaces.push(space);
        self.spaces.last().unwrap().handle()
    }

    /// 只读访问所有地址空间（用于 inspect / monitor）。
    pub fn spaces(&self) -> &[KernelAddressSpace<B>] {
        &self.spaces
    }

    pub fn get(&self, handle: AddressSpaceHandle) -> Option<&KernelAddressSpace<B>> {
        self.spaces
            .iter()
            .find(|s| s.id == handle.id && s.generation == handle.generation)
    }

    pub fn get_mut(&mut self, handle: AddressSpaceHandle) -> Option<&mut KernelAddressSpace<B>> {
        self.spaces
            .iter_mut()
            .find(|s| s.id == handle.id && s.generation == handle.generation)
    }

    /// 在已登记的空间上落一段映射（Core 验证 → 后端写 PTE → Core 记录真相）。
    pub fn map(&mut self, handle: AddressSpaceHandle, mapping: Mapping) -> Result<(), MapError> {
        self.get_mut(handle)
            .ok_or(MapError::NoSuchSpace)?
            .map(mapping)
    }

    /// 按**精确区间**解映射（见 [`KernelAddressSpace::unmap`]）。
    pub fn unmap(
        &mut self,
        handle: AddressSpaceHandle,
        range: &VirtualRange,
    ) -> Result<(), MapError> {
        self.get_mut(handle)
            .ok_or(MapError::NoSuchSpace)?
            .unmap(range)
    }

    /// 精确查询一条已记录映射（只读；返回 `Copy` 快照）。
    pub fn mapping_exact(
        &self,
        handle: AddressSpaceHandle,
        range: &VirtualRange,
    ) -> Result<Option<Mapping>, MapError> {
        Ok(self
            .get(handle)
            .ok_or(MapError::NoSuchSpace)?
            .mapping_exact(range)
            .copied())
    }

    /// 翻译虚拟地址；未知句柄是错误，未映射地址是 `Ok(None)`。
    pub fn translate(
        &self,
        handle: AddressSpaceHandle,
        va: usize,
    ) -> Result<Option<usize>, MapError> {
        Ok(self.get(handle).ok_or(MapError::NoSuchSpace)?.translate(va))
    }

    /// 准备激活描述符（持锁取一次，之后不再触碰本管理器）。
    pub fn prepare_activation(
        &self,
        handle: AddressSpaceHandle,
    ) -> Result<PreparedActivation<B::Activation>, MapError> {
        self.get(handle)
            .ok_or(MapError::NoSuchSpace)?
            .prepare_activation()
    }

    /// 标记空间退役（幂等；未知句柄 `NoSuchSpace`）。
    pub fn retire(&mut self, handle: AddressSpaceHandle) -> Result<(), MapError> {
        self.get_mut(handle).ok_or(MapError::NoSuchSpace)?.retire();
        Ok(())
    }
}

// 真实地址空间后端 alias（`arch::AddressSpaceImpl`）只在 VM profile + 适用 target 下
// 存在（见 `os/arch/src/lib.rs`）：NoMMU profile，或 MMU profile + RISC-V target。
// 其余构建（含 host test）没有真实后端 —— 给显式失败的 stub，**绝不静默降级成 native**。
#[cfg(any(
    feature = "vm-nommu",
    all(
        feature = "vm-mmu",
        any(target_arch = "riscv32", target_arch = "riscv64")
    )
))]
mod active {
    use super::{
        AddressSpaceBackend, AddressSpaceHandle, AddressSpaceManager, MapError, Mapping,
        PreparedActivation, VirtualRange,
    };
    use crate::component::ComponentId;

    pub use arch::AddressSpaceImpl;

    type ActiveSpaces = AddressSpaceManager<AddressSpaceImpl>;

    static SPACES: spin::Mutex<ActiveSpaces> = spin::Mutex::new(ActiveSpaces::empty());

    /// 当前 profile 是否具备私有地址空间能力——直接问**已选中的 backend**
    /// （trait 可用 ≠ 隔离能力：NoMMU 也实现 `AddressSpaceBackend`）。
    pub fn isolation_capable() -> bool {
        <AddressSpaceImpl as AddressSpaceBackend>::PRIVATE_ADDRESS_SPACE
    }

    /// 为一个实例建立私有地址空间（Isolated 域）。后端用 Core 注入的 `PageAlloc`
    /// （`memory::vm_page_alloc`）分配页表页。
    ///
    /// NoMMU 等没有私有 AS 能力的 profile **显式拒绝**（`MapError::Unsupported`），
    /// 绝不把恒等翻译当成私有地址空间使用。
    pub fn create_address_space_for(owner: ComponentId) -> Result<AddressSpaceHandle, MapError> {
        if !isolation_capable() {
            return Err(MapError::Unsupported);
        }
        let backend =
            <AddressSpaceImpl as AddressSpaceBackend>::create(crate::memory::vm_page_alloc)
                .map_err(|_| MapError::BackendFailed)?;
        Ok(SPACES.lock().create(owner, backend))
    }

    /// 接管一个已存在的后端（见 [`AddressSpaceManager::adopt`]）。
    ///
    /// **当前无调用方**：boot 的长期 root 仍由 boot 的 `RuntimeVm` 按值持有；
    /// 搬迁 boot 的 backend 所有权需要先重构 boot（无 host 测试的路径），本轮
    /// 只保留这个 seam，不强行实施。
    pub fn adopt(
        owner: ComponentId,
        backend: AddressSpaceImpl,
        mappings: alloc::vec::Vec<Mapping>,
    ) -> AddressSpaceHandle {
        SPACES.lock().adopt(owner, backend, mappings)
    }

    /// 在已建立的地址空间上落一段映射（Core 验证 → 后端写 PTE → Core 记录真相）。
    pub fn map(handle: AddressSpaceHandle, mapping: Mapping) -> Result<(), MapError> {
        SPACES.lock().map(handle, mapping)
    }

    /// 按精确区间解映射（release 路径按"当初 acquire 的精确 extent"回找）。
    pub fn unmap(handle: AddressSpaceHandle, range: &VirtualRange) -> Result<(), MapError> {
        SPACES.lock().unmap(handle, range)
    }

    /// 准备激活描述符（持锁取一次；消费方不再触碰本管理器）。
    pub fn prepare_activation(
        handle: AddressSpaceHandle,
    ) -> Result<PreparedActivation<<AddressSpaceImpl as AddressSpaceBackend>::Activation>, MapError>
    {
        SPACES.lock().prepare_activation(handle)
    }

    /// 标记空间退役（release 后不再复用）。
    pub fn retire(handle: AddressSpaceHandle) -> Result<(), MapError> {
        SPACES.lock().retire(handle)
    }
}

#[cfg(any(
    feature = "vm-nommu",
    all(
        feature = "vm-mmu",
        any(target_arch = "riscv32", target_arch = "riscv64")
    )
))]
pub use active::{
    AddressSpaceImpl, adopt, create_address_space_for, isolation_capable, map, prepare_activation,
    retire, unmap,
};

/// 无后端构建（host test）：没有可用的私有地址空间实现——能力恒为 `false`，
/// 绝不静默降级成"native 也能跑"。`adopt` / `unmap` / `retire` /
/// `prepare_activation` 只对真实 backend 有意义，因此不在本构建暴露。
#[cfg(not(any(
    feature = "vm-nommu",
    all(
        feature = "vm-mmu",
        any(target_arch = "riscv32", target_arch = "riscv64")
    )
)))]
pub fn isolation_capable() -> bool {
    false
}

#[cfg(not(any(
    feature = "vm-nommu",
    all(
        feature = "vm-mmu",
        any(target_arch = "riscv32", target_arch = "riscv64")
    )
)))]
pub fn create_address_space_for(_owner: ComponentId) -> Result<AddressSpaceHandle, MapError> {
    Err(MapError::BackendFailed)
}

#[cfg(not(any(
    feature = "vm-nommu",
    all(
        feature = "vm-mmu",
        any(target_arch = "riscv32", target_arch = "riscv64")
    )
)))]
pub fn map(_handle: AddressSpaceHandle, _mapping: Mapping) -> Result<(), MapError> {
    Err(MapError::BackendFailed)
}
#[allow(dead_code)]
const fn _handle_shape(id: AddressSpaceId, generation: u32) -> AddressSpaceHandle {
    AddressSpaceHandle { id, generation }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{vec, vec::Vec};

    // -- FakeBackend：记录调用，可在宿主上锁定 Core invariant -------------
    /// 测试 fixture 的 VM 对齐常量：与 FakeBackend::GRANULE 一致（4 KiB），
    /// 与分配器 `ALLOC_GRANULE` 无耦合（这正是要验证的解耦）。
    const VM_PAGE: usize = 4096;

    /// 测试用激活 token：backend 私有的切换数据快照（`Copy`，不携带借用）。
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct FakeActivation {
        mapped_count: usize,
        asid: u16,
    }

    struct FakeBackend {
        mapped: Vec<(VirtualRange, PhysicalRange, MappingPermission)>,
        unmapped: Vec<VirtualRange>,
        fail_map: bool,
        fail_unmap: bool,
        /// `activate()` 被调用的次数：证明 Core 的 map / unmap / prepare / retire
        /// 路径**从不**隐式写 satp（本阶段没有任何运行期切换）。`Cell` 是因为
        /// backend 契约的 `activate(&self)` 是只读签名。
        activations: core::cell::Cell<usize>,
    }

    impl FakeBackend {
        fn new() -> Self {
            Self {
                mapped: Vec::new(),
                unmapped: Vec::new(),
                fail_map: false,
                fail_unmap: false,
                activations: core::cell::Cell::new(0),
            }
        }
    }

    impl AddressSpaceBackend for FakeBackend {
        type Error = ();
        const GRANULE: usize = VM_PAGE;
        /// 测试 backend 模拟"具备私有 AS 机制"的翻译后端。
        const PRIVATE_ADDRESS_SPACE: bool = true;
        type Activation = FakeActivation;

        fn create(_alloc: arch::vm::PageAlloc) -> Result<Self, ()> {
            Ok(Self::new())
        }

        fn map(
            &mut self,
            va: VirtualRange,
            pa: PhysicalRange,
            perm: MappingPermission,
        ) -> Result<(), ()> {
            if self.fail_map {
                return Err(());
            }
            self.mapped.push((va, pa, perm));
            Ok(())
        }

        fn unmap(&mut self, va: VirtualRange) -> Result<(), ()> {
            if self.fail_unmap {
                return Err(());
            }
            self.unmapped.push(va);
            Ok(())
        }

        fn translate(&self, _va: usize) -> Option<usize> {
            None
        }

        fn activate(&self) -> Result<(), ()> {
            self.activations.set(self.activations.get() + 1);
            Ok(())
        }

        fn prepare_activation(&self) -> Self::Activation {
            // 只读快照：不发地址、不改状态、不写 satp。
            FakeActivation {
                mapped_count: self.mapped.len(),
                asid: 7,
            }
        }
    }

    // -- helpers ------------------------------------------------------------

    fn rw() -> MappingPermission {
        MappingPermission::READ | MappingPermission::WRITE
    }

    fn mapping(base: usize, size: usize, perm: MappingPermission) -> Mapping {
        Mapping {
            virtual_range: VirtualRange { base, size },
            physical_range: PhysicalRange { base, size },
            permission: perm,
        }
    }

    fn space(backend: FakeBackend) -> KernelAddressSpace<FakeBackend> {
        KernelAddressSpace::new(
            AddressSpaceId::from_raw(1),
            1,
            ComponentId::from_raw(1),
            backend,
        )
    }

    // -- validate ------------------------------------------------------------

    #[test]
    fn validate_rejects_empty() {
        let mut s = space(FakeBackend::new());
        assert_eq!(s.map(mapping(0x1000, 0, rw())), Err(MapError::EmptyRange));
    }

    #[test]
    fn validate_rejects_length_mismatch() {
        let mut s = space(FakeBackend::new());
        let mut m = mapping(0x1000, 0x1000, rw());
        m.physical_range = PhysicalRange {
            base: 0x2000,
            size: 0x2000,
        };
        assert_eq!(s.map(m), Err(MapError::LengthMismatch));
    }

    #[test]
    fn validate_rejects_unaligned() {
        let mut s = space(FakeBackend::new());
        assert_eq!(
            s.map(mapping(0x1001, 0x1000, rw())),
            Err(MapError::Unaligned)
        );
    }

    #[test]
    fn validate_rejects_overflow() {
        let mut s = space(FakeBackend::new());
        // base + size 溢出回绕；必须被拦下，否则 backend 会一页不写却报成功。
        let m = mapping(0xffff_ffff_ffff_f000, 0x2000, rw());
        assert_eq!(s.map(m), Err(MapError::AddressOverflow));
    }

    #[test]
    fn validate_rejects_overlap() {
        let mut s = space(FakeBackend::new());
        s.map(mapping(0x1000, 0x3000, rw())).unwrap();
        assert_eq!(s.map(mapping(0x2000, 0x1000, rw())), Err(MapError::Overlap));
    }

    // -- backend GRANULE ≠ allocator ALLOC_GRANULE 的解耦证明 ----------------

    /// 8 KiB 粒度 backend：证明 Core 校验跟随 backend 自己的规则，
    /// 与分配器 `ALLOC_GRANULE`（4 KiB）在语义上无关。
    struct EightKBackend;

    impl AddressSpaceBackend for EightKBackend {
        type Error = ();
        const GRANULE: usize = 0x2000; // 8 KiB，故意 ≠ ALLOC_GRANULE
        const PRIVATE_ADDRESS_SPACE: bool = true;
        type Activation = FakeActivation;

        fn create(_alloc: arch::vm::PageAlloc) -> Result<Self, ()> {
            Ok(EightKBackend)
        }

        fn map(
            &mut self,
            _va: VirtualRange,
            _pa: PhysicalRange,
            _perm: MappingPermission,
        ) -> Result<(), ()> {
            Ok(())
        }

        fn unmap(&mut self, _va: VirtualRange) -> Result<(), ()> {
            Ok(())
        }

        fn translate(&self, _va: usize) -> Option<usize> {
            None
        }

        fn activate(&self) -> Result<(), ()> {
            Ok(())
        }

        fn prepare_activation(&self) -> Self::Activation {
            FakeActivation {
                mapped_count: 0,
                asid: 0,
            }
        }
    }

    #[test]
    fn vm_alignment_follows_backend_granule_not_alloc_granule() {
        // 4 KiB 对齐（满足 ALLOC_GRANULE）但非 8 KiB 对齐 → 必须被拒绝：
        // 证明 Core 用的是 backend GRANULE，不是分配器常量。
        let mut s = KernelAddressSpace::<EightKBackend>::new(
            AddressSpaceId::from_raw(1),
            1,
            ComponentId::from_raw(1),
            EightKBackend,
        );
        assert_eq!(
            s.map(mapping(0x1000, 0x1000, rw())),
            Err(MapError::Unaligned),
            "4K 对齐但非 backend(8K) 对齐的映射必须被拒绝"
        );
        // 8 KiB 对齐 → 放行（同一个 Core 校验函数，仅 backend 不同）。
        assert!(s.map(mapping(0x2000, 0x2000, rw())).is_ok());
    }

    // -- map / commit ----------------------------------------------------------

    #[test]
    fn map_commits_on_success() {
        let mut s = space(FakeBackend::new());
        assert!(s.map(mapping(0x1000, 0x1000, rw())).is_ok());
        assert_eq!(s.mappings().len(), 1);
        assert_eq!(s.backend.mapped.len(), 1);
    }

    #[test]
    fn backend_failure_does_not_commit() {
        let mut backend = FakeBackend::new();
        backend.fail_map = true;
        let mut s = space(backend);
        assert_eq!(
            s.map(mapping(0x1000, 0x1000, rw())),
            Err(MapError::BackendFailed)
        );
        assert_eq!(s.mappings().len(), 0);
    }

    // -- unmap ---------------------------------------------------------------

    #[test]
    fn unmap_exact_removes_mapping() {
        let mut s = space(FakeBackend::new());
        s.map(mapping(0x1000, 0x3000, rw())).unwrap();
        let range = VirtualRange {
            base: 0x1000,
            size: 0x3000,
        };
        assert!(s.unmap(&range).is_ok());
        assert_eq!(s.mappings().len(), 0);
        assert_eq!(s.backend.unmapped, vec![range]);
    }

    #[test]
    fn unmap_partial_range_is_not_mapped() {
        let mut s = space(FakeBackend::new());
        s.map(mapping(0x1000, 0x3000, rw())).unwrap();
        // v1 不支持 partial unmap：子区间返回 NotMapped，不触碰后端与真相。
        let sub = VirtualRange {
            base: 0x2000,
            size: 0x1000,
        };
        assert_eq!(s.unmap(&sub), Err(MapError::NotMapped));
        assert_eq!(s.mappings().len(), 1);
        assert!(s.backend.unmapped.is_empty());
    }

    #[test]
    fn unmap_unknown_returns_not_mapped() {
        let mut s = space(FakeBackend::new());
        let range = VirtualRange {
            base: 0x9000,
            size: 0x1000,
        };
        assert_eq!(s.unmap(&range), Err(MapError::NotMapped));
    }

    #[test]
    fn backend_unmap_failure_keeps_truth() {
        let mut s = space(FakeBackend::new());
        s.map(mapping(0x1000, 0x1000, rw())).unwrap();
        assert_eq!(s.mappings().len(), 1);
        s.backend.fail_unmap = true;
        let range = VirtualRange {
            base: 0x1000,
            size: 0x1000,
        };
        assert_eq!(s.unmap(&range), Err(MapError::BackendFailed));
        assert_eq!(s.mappings().len(), 1);
    }

    // -- mid-map 失败：Core 不得半提交（validation/rollback）------------------

    /// 模拟"多页映射写到一半失败"的后端：已经推进的页数是 backend 内部进度
    /// （真实页表 backend 负责回滚），Core 能保证的是**真相不半提交**。
    struct MidMapFailBackend {
        progressed_pages: usize,
        fail: bool,
        committed: Vec<(VirtualRange, PhysicalRange, MappingPermission)>,
    }

    impl MidMapFailBackend {
        fn new() -> Self {
            Self {
                progressed_pages: 0,
                fail: true,
                committed: Vec::new(),
            }
        }
    }

    impl AddressSpaceBackend for MidMapFailBackend {
        type Error = ();
        const GRANULE: usize = VM_PAGE;
        const PRIVATE_ADDRESS_SPACE: bool = true;
        type Activation = FakeActivation;

        fn create(_alloc: arch::vm::PageAlloc) -> Result<Self, ()> {
            Ok(Self::new())
        }

        fn map(
            &mut self,
            va: VirtualRange,
            pa: PhysicalRange,
            perm: MappingPermission,
        ) -> Result<(), ()> {
            // 逐页推进（模拟写 PTE）；中途失败时已推进的页保留在 backend 内部。
            for _ in 0..(va.size / VM_PAGE) {
                self.progressed_pages += 1;
                if self.fail {
                    return Err(());
                }
            }
            self.committed.push((va, pa, perm));
            Ok(())
        }

        fn unmap(&mut self, va: VirtualRange) -> Result<(), ()> {
            self.committed.retain(|(range, _, _)| *range != va);
            Ok(())
        }

        fn translate(&self, _va: usize) -> Option<usize> {
            None
        }

        fn activate(&self) -> Result<(), ()> {
            Ok(())
        }

        fn prepare_activation(&self) -> Self::Activation {
            FakeActivation {
                mapped_count: self.committed.len(),
                asid: 0,
            }
        }
    }

    #[test]
    fn mid_map_failure_is_not_committed_and_range_stays_reusable() {
        // Given：一个 3 页映射，backend 在第 1 页后失败（多页中途失败）。
        let mut s = KernelAddressSpace::<MidMapFailBackend>::new(
            AddressSpaceId::from_raw(1),
            1,
            ComponentId::from_raw(1),
            MidMapFailBackend::new(),
        );
        let range = VirtualRange {
            base: 0x4000,
            size: 3 * VM_PAGE,
        };
        let m = Mapping {
            virtual_range: range,
            physical_range: PhysicalRange {
                base: 0x4000,
                size: 3 * VM_PAGE,
            },
            permission: rw(),
        };

        // When：中途失败。
        assert_eq!(s.map(m), Err(MapError::BackendFailed));

        // Then：backend 确实做了部分工作，但 Core 真相**零提交**——
        // "半条映射"绝不能进入 ledger。
        assert!(s.backend.progressed_pages > 0, "backend 已部分推进");
        assert!(s.mappings().is_empty(), "失败不得提交半条映射");
        assert!(s.mapping_exact(&range).is_none());

        // 同一区间再试：backend 恢复后成功提交（失败不污染后续状态）。
        s.backend.fail = false;
        assert_eq!(s.map(m), Ok(()));
        assert_eq!(s.mappings(), &[m]);
    }

    // -- 精确查询 / 管理器生命周期 --------------------------------------------

    #[test]
    fn mapping_exact_matches_only_the_identical_range() {
        let mut manager: AddressSpaceManager<FakeBackend> = AddressSpaceManager::empty();
        let handle = manager.create(ComponentId::from_raw(1), FakeBackend::new());
        let full = VirtualRange {
            base: 0x1000,
            size: 0x3000,
        };
        let m = mapping(full.base, full.size, rw());
        manager.map(handle, m).unwrap();

        // 精确区间命中；子区间 / 超集 / 未映射都返回 None（不做包含匹配）。
        assert_eq!(manager.mapping_exact(handle, &full).unwrap(), Some(m));
        assert_eq!(
            manager
                .mapping_exact(
                    handle,
                    &VirtualRange {
                        base: 0x2000,
                        size: 0x1000
                    }
                )
                .unwrap(),
            None
        );
        assert_eq!(
            manager
                .mapping_exact(
                    handle,
                    &VirtualRange {
                        base: 0x1000,
                        size: 0x4000
                    }
                )
                .unwrap(),
            None
        );

        // 未知句柄是错误（不是"未映射"）。
        let ghost = AddressSpaceHandle {
            id: AddressSpaceId::from_raw(999),
            generation: 1,
        };
        assert_eq!(
            manager.mapping_exact(ghost, &full),
            Err(MapError::NoSuchSpace)
        );

        // 精确 unmap：整段成功、ledger 移除；再次 unmap 同区间 → NotMapped。
        manager.unmap(handle, &full).unwrap();
        assert_eq!(manager.mapping_exact(handle, &full).unwrap(), None);
        assert_eq!(manager.unmap(handle, &full), Err(MapError::NotMapped));
    }

    #[test]
    fn retired_space_rejects_operations_and_translate() {
        let mut manager: AddressSpaceManager<FakeBackend> = AddressSpaceManager::empty();
        let handle = manager.create(ComponentId::from_raw(1), FakeBackend::new());
        let range = VirtualRange {
            base: 0x1000,
            size: 0x1000,
        };
        manager
            .map(handle, mapping(range.base, range.size, rw()))
            .unwrap();
        assert_eq!(
            manager.translate(handle, 0x1000).unwrap(),
            None,
            "Fake 无翻译"
        );

        // When：退役。
        manager.retire(handle).unwrap();
        assert_eq!(
            manager.get(handle).unwrap().state(),
            AddressSpaceState::Retired
        );

        // Then：所有 mutation / 激活准备一律拒绝；translate 返回 None；退役幂等。
        assert_eq!(
            manager.map(handle, mapping(0x2000, 0x1000, rw())),
            Err(MapError::Retired)
        );
        assert_eq!(manager.unmap(handle, &range), Err(MapError::Retired));
        assert_eq!(manager.prepare_activation(handle), Err(MapError::Retired));
        assert_eq!(manager.translate(handle, 0x1000).unwrap(), None);
        assert_eq!(
            manager.mapping_exact(handle, &range).unwrap(),
            Some(mapping(range.base, range.size, rw())),
            "只读查询仍可见历史真相"
        );
        assert_eq!(manager.retire(handle), Ok(()), "退役幂等");

        // 未知句柄：拒绝（NoSuchSpace），不是静默 no-op。
        let ghost = AddressSpaceHandle {
            id: AddressSpaceId::from_raw(999),
            generation: 1,
        };
        assert_eq!(manager.retire(ghost), Err(MapError::NoSuchSpace));
        assert_eq!(
            manager.prepare_activation(ghost),
            Err(MapError::NoSuchSpace)
        );
    }

    #[test]
    fn prepared_activation_is_a_copy_snapshot_and_never_writes_satp() {
        let mut manager: AddressSpaceManager<FakeBackend> = AddressSpaceManager::empty();
        let handle = manager.create(ComponentId::from_raw(1), FakeBackend::new());
        manager.map(handle, mapping(0x1000, 0x1000, rw())).unwrap();

        // When：持锁取一次描述符（快照）。
        let prepared = manager.prepare_activation(handle).unwrap();

        // Then：描述符携带空间身份 + backend 私有 token；`Copy` 可复用。
        assert_eq!(prepared.handle(), handle);
        assert_eq!(
            prepared.token(),
            FakeActivation {
                mapped_count: 1,
                asid: 7
            }
        );
        let copy = prepared;

        // 之后继续改空间：描述符是**快照**，不受影响（消费方无需再碰管理器）。
        manager.map(handle, mapping(0x2000, 0x1000, rw())).unwrap();
        assert_eq!(copy.token(), prepared.token());
        assert_eq!(
            manager.prepare_activation(handle).unwrap().token(),
            FakeActivation {
                mapped_count: 2,
                asid: 7
            }
        );

        // prepare 从不隐式激活：没有 satp 写入、没有 TLB 刷新。
        assert_eq!(manager.get(handle).unwrap().backend.activations.get(), 0);
    }

    /// 地址空间生命周期**不涉及任何组件执行**：用从未在 registry 声明的 owner
    /// 跑完 create → map → query → prepare → unmap → retire，全程零组件入口、
    /// 零 containment 边界、零隐式 satp 写。真实后端激活（increment 3）不在这
    /// 条路径上——本用例锁定"increment 2 只准备、不切换"。
    #[test]
    fn address_space_lifecycle_needs_no_component_execution() {
        let owner = ComponentId::from_raw(0xDEAD_BEEF); // 未声明的身份也无所谓
        let mut manager: AddressSpaceManager<FakeBackend> = AddressSpaceManager::empty();
        let handle = manager.create(owner, FakeBackend::new());

        let range = VirtualRange {
            base: 0x8000,
            size: 2 * VM_PAGE,
        };
        manager
            .map(handle, mapping(range.base, range.size, rw()))
            .unwrap();
        assert!(manager.mapping_exact(handle, &range).unwrap().is_some());
        let prepared = manager.prepare_activation(handle).unwrap();
        manager.unmap(handle, &range).unwrap();
        manager.retire(handle).unwrap();

        // 全程没有触发任何 backend.activate()；退役后连描述符都取不到。
        assert_eq!(manager.get(handle).unwrap().backend.activations.get(), 0);
        assert_eq!(manager.prepare_activation(handle), Err(MapError::Retired));
        assert_eq!(prepared.handle(), handle, "已取到的描述符仍可用于后续切换");
    }

    /// `adopt`：接管一个已存在 backend，连同调用方声明的既有映射一起登记
    /// （boot root 登记 hook 的语义锁定；boot 尚未调用，见管理器文档）。
    #[test]
    fn adopt_registers_existing_backend_with_declared_mappings() {
        let mut manager: AddressSpaceManager<FakeBackend> = AddressSpaceManager::empty();
        let declared = mapping(0x9000, 0x1000, rw());
        let handle = manager.adopt(
            ComponentId::from_raw(2),
            FakeBackend::new(),
            alloc::vec![declared],
        );

        assert_eq!(
            manager.get(handle).unwrap().owner(),
            ComponentId::from_raw(2)
        );
        assert_eq!(
            manager.get(handle).unwrap().state(),
            AddressSpaceState::Ready
        );
        assert_eq!(
            manager
                .mapping_exact(handle, &declared.virtual_range)
                .unwrap(),
            Some(declared),
            "Core 记录调用方声明的既有映射真相（不探测 backend）"
        );
        // 接管的空 backend 与声明清单不一致是调用方的问题；Core 不做探测。
        assert!(manager.get(handle).unwrap().backend.mapped.is_empty());
    }

    // -- accessors -----------------------------------------------------------

    #[test]
    fn accessors_expose_read_only_views() {
        let mut s = space(FakeBackend::new());
        s.map(mapping(0x1000, 0x1000, rw())).unwrap();
        assert_eq!(s.id(), AddressSpaceId::from_raw(1));
        assert_eq!(s.owner(), ComponentId::from_raw(1));
        assert_eq!(s.state(), AddressSpaceState::Ready);
        assert_eq!(s.mappings().len(), 1);
        let _ = s.handle();
    }

    // -- Property tests（Invariant A–D，docs/development/testing.md §5）---------------------
    //
    // A: 任意时刻 ledger 中不存在 VA overlap
    // B: 失败操作后 Core truth == 操作前 Core truth（ledger 与 backend 都不变）
    // C: 成功操作后 ledger 与 FakeBackend 观察结果一致
    // D: 任意序列不 panic
    //
    // 用 proptest 生成随机 map/unmap 序列 + 随机 backend 成败，逐操作验证。

    use proptest::prelude::*;

    #[derive(Debug, Clone, Copy)]
    enum OpKind {
        Map {
            base: usize,
            pages: usize,
            perm: MappingPermission,
            backend_fail: bool,
        },
        Unmap {
            base: usize,
            pages: usize,
            backend_fail: bool,
        },
    }

    fn aligned_base() -> impl Strategy<Value = usize> {
        // 上限压小，让序列里容易产生 overlap / 相邻区间
        (0usize..0x1000).prop_map(|pages| pages * VM_PAGE)
    }

    /// 序列生成器：随机 map（随机权限 + 随机 backend 失败）与 unmap。
    fn op_seq() -> impl Strategy<Value = Vec<OpKind>> {
        proptest::collection::vec(op_kind_strategy(), 1..=40)
    }

    fn op_kind_strategy() -> impl Strategy<Value = OpKind> {
        let pages = 1usize..=4;
        let perm = prop_oneof![
            Just(MappingPermission::READ),
            Just(MappingPermission::READ | MappingPermission::WRITE),
            Just(MappingPermission::READ | MappingPermission::EXECUTE),
            Just(MappingPermission::READ | MappingPermission::WRITE | MappingPermission::EXECUTE),
        ];
        prop_oneof![
            (aligned_base(), pages.clone(), perm, any::<bool>()).prop_map(
                |(base, pages, perm, fail)| OpKind::Map {
                    base,
                    pages,
                    perm,
                    backend_fail: fail,
                }
            ),
            (aligned_base(), pages, any::<bool>()).prop_map(|(base, pages, fail)| OpKind::Unmap {
                base,
                pages,
                backend_fail: fail,
            }),
        ]
    }

    fn size_of(pages: usize) -> usize {
        pages * VM_PAGE
    }

    /// Invariant A：ledger 内无 VA overlap（相邻允许，重叠禁止）。
    fn assert_no_overlap(mappings: &[Mapping]) {
        let mut sorted: Vec<VirtualRange> = mappings.iter().map(|m| m.virtual_range).collect();
        sorted.sort_by_key(|r| r.base);
        for pair in sorted.windows(2) {
            let a = pair[0];
            let b = pair[1];
            assert!(a.base + a.size <= b.base, "VA overlap: {a:?} vs {b:?}");
        }
    }

    fn apply_and_check(space: &mut KernelAddressSpace<FakeBackend>, op: OpKind) {
        let mappings_before: Vec<Mapping> = space.mappings().to_vec();
        let backend_before = (space.backend.mapped.clone(), space.backend.unmapped.clone());

        match op {
            OpKind::Map {
                base,
                pages,
                perm,
                backend_fail,
            } => {
                let m = mapping(base, size_of(pages), perm);
                space.backend.fail_map = backend_fail;
                let result = space.map(m);
                match result {
                    Ok(()) => {
                        // C: backend 收到精确参数，ledger 与 backend 一致
                        assert_eq!(
                            space.mappings().len(),
                            mappings_before.len() + 1,
                            "成功 map 必须提交一条"
                        );
                        let last = space.backend.mapped.last().expect("backend got the map");
                        assert_eq!(*last, (m.virtual_range, m.physical_range, m.permission));
                        assert!(space.mappings().contains(&m));
                    }
                    Err(e) => {
                        // B: 失败后 Core truth 不变（ledger + backend 都保持原样）
                        assert_eq!(
                            space.mappings(),
                            mappings_before.as_slice(),
                            "失败后 ledger 不得变化 (err={e:?})"
                        );
                        assert_eq!(space.backend.mapped, backend_before.0);
                        assert_eq!(space.backend.unmapped, backend_before.1);
                    }
                }
            }
            OpKind::Unmap {
                base,
                pages,
                backend_fail,
            } => {
                let range = VirtualRange {
                    base,
                    size: size_of(pages),
                };
                space.backend.fail_unmap = backend_fail;
                let result = space.unmap(&range);
                match result {
                    Ok(()) => {
                        // C: backend 收到整段 unmap，ledger 移除对应记录
                        assert!(space.backend.unmapped.contains(&range));
                        assert!(!space.mappings().iter().any(|m| m.virtual_range == range));
                    }
                    Err(_) => {
                        assert_eq!(space.mappings(), mappings_before.as_slice());
                        assert_eq!(space.backend.mapped, backend_before.0);
                        assert_eq!(space.backend.unmapped, backend_before.1);
                    }
                }
            }
        }
        // A: 每步之后 ledger 无 overlap
        assert_no_overlap(space.mappings());
    }

    proptest! {
        #[test]
        fn random_sequences_keep_invariants(ops in op_seq()) {
            let mut space = space(FakeBackend::new());
            for op in ops {
                apply_and_check(&mut space, op);
            }
        }
    }

    #[test]
    fn property_test_helpers_are_sane() {
        // 防呆：op_seq 生成器本身必须产生合法 base（页对齐）
        let mut space = space(FakeBackend::new());
        apply_and_check(
            &mut space,
            OpKind::Map {
                base: VM_PAGE,
                pages: 2,
                perm: rw(),
                backend_fail: false,
            },
        );
        assert_eq!(space.mappings().len(), 1);
        assert_no_overlap(space.mappings());
        apply_and_check(
            &mut space,
            OpKind::Unmap {
                base: VM_PAGE,
                pages: 2,
                backend_fail: false,
            },
        );
        assert_eq!(space.mappings().len(), 0);
    }

    /// GRANULE=1 时，Core 的 `validate` 必须放行非 4K 对齐区间——
    /// 这是"Core 不依赖 MMU"的硬证据（对应 roadmap 的 NoMMU 验收点）。
    #[cfg(feature = "vm-nommu")]
    #[test]
    fn core_validation_accepts_unaligned_with_granule_one() {
        use arch::nommu::NoMmuAddressSpace;

        let mut space = KernelAddressSpace::new(
            AddressSpaceId::from_raw(1),
            1,
            ComponentId::from_raw(1),
            NoMmuAddressSpace,
        );
        let range = VirtualRange {
            base: 0x1005,
            size: 0x1000,
        };
        assert_eq!(
            space.map(Mapping {
                virtual_range: range,
                physical_range: PhysicalRange {
                    base: 0x1005,
                    size: 0x1000,
                },
                permission: rw(),
            }),
            Ok(())
        );
        assert_eq!(
            space.mappings(),
            &[Mapping {
                virtual_range: range,
                physical_range: PhysicalRange {
                    base: 0x1005,
                    size: 0x1000,
                },
                permission: rw(),
            }]
        );
    }

    /// 性能基线（`make bench`）：**Core 语义 ledger 成本** vs **完整路径成本**。
    ///
    /// `validate` 是私有的，只有 crate 内的 benchmark 能单独测到它 —— 这正是
    /// "先测 ledger、再测 backend" 的前提。真实页表 backend 的成本必须用 arch
    /// 后端在目标端测（见 docs/development/benchmark.md §6）。
    #[test]
    #[ignore = "性能基线：make bench 手动跑"]
    fn bench_address_space_paths() {
        let mut space = space(FakeBackend::new());
        let probe = mapping(0x1000, VM_PAGE, rw());

        crate::bench::report_environment();

        // 纯验证：只读 ledger，不改变任何状态。
        crate::bench::run("address_space.validate", 1_000, || {
            space.validate(&probe).is_ok()
        })
        .report();

        // 完整路径：validate + backend.map + ledger.commit，再 unmap 回来
        // （否则 ledger 会无限增长，测出来的是内存压力而不是 map 成本）。
        let va = VirtualRange {
            base: 0x2000,
            size: VM_PAGE,
        };
        crate::bench::run("address_space.map_unmap", 1_000, || {
            space.map(mapping(0x2000, VM_PAGE, rw())).unwrap();
            space.unmap(&va).unwrap()
        })
        .report();
    }
}
