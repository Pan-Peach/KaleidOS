//! Runtime address-space vocabulary and ownership skeleton.
//!
//! This module intentionally contains semantic Core state only. The concrete
//! translation representation belongs to an architecture backend.

use crate::component::ComponentId;
use crate::memory::PAGE_SIZE;

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
    Ready,
    Dying,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    EmptyRange,
    LengthMismatch,
    Unaligned,
    Overlap,
    AddressOverflow,
    NotMapped,
    BackendFailed,
}

fn is_page_aligned(addr: usize) -> bool {
    addr & (PAGE_SIZE - 1) == 0
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
        if !is_page_aligned(vr.base) || !is_page_aligned(pr.base) {
            return Err(MapError::Unaligned);
        }
        if !is_page_aligned(vr.size) || !is_page_aligned(pr.size) {
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
    pub fn map(&mut self, mapping: Mapping) -> Result<(), MapError> {
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
    pub fn unmap(&mut self, range: &VirtualRange) -> Result<(), MapError> {
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
    pub fn translate(&self, va: usize) -> Option<usize> {
        self.backend.translate(va)
    }

    /// 把本地址空间激活为当前 satp（委托后端写 satp + sfence）。
    pub fn activate(&self) -> Result<(), MapError> {
        self.backend.activate().map_err(|_| MapError::BackendFailed)
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
        let id = AddressSpaceId::from_raw(self.next_id);
        self.next_id += 1;
        self.spaces
            .push(KernelAddressSpace::new(id, 1, owner, backend));
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

    struct FakeBackend {
        mapped: Vec<(VirtualRange, PhysicalRange, MappingPermission)>,
        unmapped: Vec<VirtualRange>,
        fail_map: bool,
        fail_unmap: bool,
    }

    impl FakeBackend {
        fn new() -> Self {
            Self {
                mapped: Vec::new(),
                unmapped: Vec::new(),
                fail_map: false,
                fail_unmap: false,
            }
        }
    }

    impl AddressSpaceBackend for FakeBackend {
        type Error = ();

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
            Ok(())
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

    // -- Property tests（Invariant A–D，docs/testing.md §5）---------------------
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
        },
    }

    fn aligned_base() -> impl Strategy<Value = usize> {
        // 上限压小，让序列里容易产生 overlap / 相邻区间
        (0usize..0x1000).prop_map(|pages| pages * PAGE_SIZE)
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
            Just(
                MappingPermission::READ
                    | MappingPermission::WRITE
                    | MappingPermission::EXECUTE
            ),
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
            (aligned_base(), pages)
                .prop_map(|(base, pages)| OpKind::Unmap { base, pages }),
        ]
    }

    fn size_of(pages: usize) -> usize {
        pages * PAGE_SIZE
    }

    /// Invariant A：ledger 内无 VA overlap（相邻允许，重叠禁止）。
    fn assert_no_overlap(mappings: &[Mapping]) {
        let mut sorted: Vec<VirtualRange> = mappings
            .iter()
            .map(|m| m.virtual_range)
            .collect();
        sorted.sort_by_key(|r| r.base);
        for pair in sorted.windows(2) {
            let a = pair[0];
            let b = pair[1];
            assert!(
                a.base + a.size <= b.base,
                "VA overlap: {a:?} vs {b:?}"
            );
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
            OpKind::Unmap { base, pages } => {
                let range = VirtualRange {
                    base,
                    size: size_of(pages),
                };
                let result = space.unmap(&range);
                match result {
                    Ok(()) => {
                        // C: backend 收到整段 unmap，ledger 移除对应记录
                        assert!(space.backend.unmapped.contains(&range));
                        assert!(!space
                            .mappings()
                            .iter()
                            .any(|m| m.virtual_range == range));
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
        apply_and_check(&mut space, OpKind::Map {
            base: PAGE_SIZE,
            pages: 2,
            perm: rw(),
            backend_fail: false,
        });
        assert_eq!(space.mappings().len(), 1);
        assert_no_overlap(space.mappings());
        apply_and_check(&mut space, OpKind::Unmap {
            base: PAGE_SIZE,
            pages: 2,
        });
        assert_eq!(space.mappings().len(), 0);
    }
}
