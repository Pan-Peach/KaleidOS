//! 共享 Core 映射计划：**每个 Isolated AS 与内核 root 保持 same VA → same PA**
//! 的映射集合，以及**私有 backing 的别名排除**（alias exclusion）事务。
//!
//! 设计（`docs/architecture/deployment.md` §6.3 / `memory-and-heap.md` §8）：
//!
//! - Isolated 组件保持 S-mode，Core 代码 / 栈 / 全局状态在每个 Isolated AS 里都
//!   必须可执行 / 可访问 → Core 把一份**从真实 boot/runtime builder 记录的**映射
//!   计划共享进每个实例 AS；`satp` 只在真正跨组件边界时切换。
//! - 计划不是"复制 boot root"：RV64 runtime 的 identity RAM 是 **RWX**、RV32
//!   bootstrap identity 覆盖全部 4 GiB——盲目共享会把别的实例的私有 backing
//!   经 identity 别名暴露出去。因此 identity RAM 以 [`MappingClass::SharedIdentity`]
//!   登记，**私有 backing 发布时先把它的 identity 别名从计划与所有活着的
//!   Isolated root 里摘掉**（[`KernelMappingPlan::exclude`] +
//!   `address_space::exclude_identity_alias`）。
//! - 这不是内存账本：只为"哪些物理 extent 已经是组件私有的"保留排除记录，
//!   归属仍由该实例的页表承载（Core 不记 owner / 不做字节计费）。
//!
//! 语义分类（由 builder 在**每次映射操作**时标注）：
//!
//! | class | 含义 | 是否进 Isolated AS |
//! |---|---|---|
//! | [`MappingClass::SharedCore`] | Core 镜像段 / 栈 / 陷阱向量 / 中断控制器 MMIO 等固定映射 | 是（同 VA→同 PA） |
//! | [`MappingClass::SharedIdentity`] | identity RAM（VA==PA）：Core 堆 / 页表页 / 组件池 | 是，**可被排除**（私有 backing 的别名必须摘掉） |
//! | [`MappingClass::Device`] | 非 Core 必需的 device MMIO | 是（同一窗口可在实例 AS 里访问） |
//! | [`MappingClass::CoreRootOnly`] | 只属于内核 root（bootstrap 影子等） | 否 |

use alloc::vec::Vec;

use super::address_space::{Mapping, PhysicalRange, VirtualRange};

/// 一次映射操作在计划里的语义分类（见模块文档）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MappingClass {
    /// 固定 Core 映射（镜像段 / 栈 / 陷阱入口 / PLIC 窗口）：所有 Isolated AS 共享。
    SharedCore,
    /// identity RAM（VA == PA）：共享，但**私有 backing 的别名必须可被摘除**。
    SharedIdentity,
    /// 一般 device MMIO：共享窗口（Core 与实例都可访问）。
    Device,
    /// 只留在内核 root（bootstrap 影子、非 Core 必需的设备窗口等）。
    CoreRootOnly,
}

/// 计划条目的物理范围与已发布私有 backing 重叠：
/// 共享映射会把私有内存暴露给别的实例——拒绝，绝不静默拆掉 Core 映射。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanError {
    /// 空区间 / size 为 0。
    Empty,
    /// 未页对齐。
    Unaligned,
    /// 地址算术溢出。
    AddressOverflow,
    /// 与已登记条目 VA 重叠（计划里不允许重叠映射：分配类映射的 alias 排除
    /// 依赖"每个字节至多一条 identity 记录"）。
    Overlap,
    /// 排除范围与非 identity 共享映射的物理范围重叠（真实别名泄漏）。
    PrivateAliasesShared,
    OutOfMemory,
}

impl From<PlanError> for crate::memory::address_space::MapError {
    fn from(error: PlanError) -> Self {
        match error {
            PlanError::OutOfMemory => Self::OutOfMemory,
            _ => Self::Unsupported,
        }
    }
}

/// 计划里的一条映射。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanEntry {
    pub class: MappingClass,
    pub mapping: Mapping,
}

/// 共享 Core 映射计划（host-testable 纯逻辑）。
#[derive(Debug, Clone, Default)]
pub struct KernelMappingPlan {
    entries: Vec<PlanEntry>,
    /// 已发布为组件私有的物理 extent（identity 别名必须摘掉，且不得进入
    /// 后续创建的 Isolated root 的共享计划）。
    exclusions: Vec<PhysicalRange>,
}

fn page_aligned(value: usize, granule: usize) -> bool {
    granule != 0 && value & (granule - 1) == 0
}

fn range_end(base: usize, size: usize) -> Option<usize> {
    base.checked_add(size)
}

impl KernelMappingPlan {
    pub const fn empty() -> Self {
        Self {
            entries: Vec::new(),
            exclusions: Vec::new(),
        }
    }

    /// 登记一次映射操作的语义分类。`granule` 是该 profile 的页粒度（4 KiB）。
    ///
    /// - 必须页对齐、非空、地址不溢出；
    /// - 计划里 VA 不允许重叠（同一段不登记两次）；
    /// - 私有 backing 的 extent 与**非 identity** 共享映射重叠 → 显式拒绝
    ///   （那是真实别名泄漏，不是可排除的 identity 别名）。
    pub fn add(
        &mut self,
        class: MappingClass,
        mapping: Mapping,
        granule: usize,
    ) -> Result<(), PlanError> {
        let vr = mapping.virtual_range;
        let pr = mapping.physical_range;
        if vr.size == 0 || pr.size == 0 {
            return Err(PlanError::Empty);
        }
        if vr.size != pr.size {
            return Err(PlanError::Empty);
        }
        if !page_aligned(vr.base, granule)
            || !page_aligned(pr.base, granule)
            || !page_aligned(vr.size, granule)
        {
            return Err(PlanError::Unaligned);
        }
        let va_end = range_end(vr.base, vr.size).ok_or(PlanError::AddressOverflow)?;
        let pa_end = range_end(pr.base, pr.size).ok_or(PlanError::AddressOverflow)?;
        for entry in &self.entries {
            let other = entry.mapping.virtual_range;
            let other_end = range_end(other.base, other.size).ok_or(PlanError::AddressOverflow)?;
            if vr.base < other_end && other.base < va_end {
                return Err(PlanError::Overlap);
            }
        }
        for exclusion in &self.exclusions {
            let ex_end =
                range_end(exclusion.base, exclusion.size).ok_or(PlanError::AddressOverflow)?;
            if class != MappingClass::SharedIdentity && pr.base < ex_end && exclusion.base < pa_end
            {
                return Err(PlanError::PrivateAliasesShared);
            }
        }
        self.entries
            .try_reserve(1)
            .map_err(|_| PlanError::OutOfMemory)?;
        self.entries.push(PlanEntry { class, mapping });
        Ok(())
    }

    /// 把一个物理 extent 标记为**组件私有**：后续 Isolated root 的共享计划
    /// 不再覆盖它；调用方还必须对**已存在的** Isolated root 摘除 identity 别名
    /// （`address_space::exclude_identity_alias`）。
    ///
    /// 排除是**整段**的：buddy order 容量（含 padding）一起排除，绝不留半个页。
    pub fn exclude(&mut self, extent: PhysicalRange, granule: usize) -> Result<(), PlanError> {
        if extent.size == 0 {
            return Err(PlanError::Empty);
        }
        if !page_aligned(extent.base, granule) || !page_aligned(extent.size, granule) {
            return Err(PlanError::Unaligned);
        }
        let ex_end = range_end(extent.base, extent.size).ok_or(PlanError::AddressOverflow)?;
        for entry in &self.entries {
            if entry.class == MappingClass::SharedIdentity {
                continue;
            }
            let pr = entry.mapping.physical_range;
            let pa_end = range_end(pr.base, pr.size).ok_or(PlanError::AddressOverflow)?;
            if extent.base < pa_end && pr.base < ex_end {
                return Err(PlanError::PrivateAliasesShared);
            }
        }
        self.exclusions
            .try_reserve(1)
            .map_err(|_| PlanError::OutOfMemory)?;
        self.exclusions.push(extent);
        Ok(())
    }

    /// 该物理 extent 是否已发布为私有。
    pub fn is_excluded(&self, extent: PhysicalRange) -> bool {
        self.exclusions.iter().any(|exclusion| {
            let Some(ex_end) = range_end(exclusion.base, exclusion.size) else {
                return false;
            };
            let Some(pa_end) = range_end(extent.base, extent.size) else {
                return false;
            };
            extent.base < ex_end && exclusion.base < pa_end
        })
    }

    /// Original identity mappings covering an exact published allocation.
    /// Reinstall these before removing the exclusion and returning the backing.
    fn restoration(&self, extent: PhysicalRange) -> Result<Vec<Mapping>, PlanError> {
        let index = self
            .exclusions
            .iter()
            .position(|excluded| *excluded == extent)
            .ok_or(PlanError::PrivateAliasesShared)?;
        let end = extent
            .base
            .checked_add(extent.size)
            .ok_or(PlanError::AddressOverflow)?;
        // A shared alias must never reopen another still-private allocation,
        // even if a caller has accidentally published overlapping exclusions.
        if self.exclusions.iter().enumerate().any(|(i, other)| {
            i != index && extent.base < other.base + other.size && other.base < end
        }) {
            return Err(PlanError::PrivateAliasesShared);
        }
        let mut mappings = Vec::new();
        for entry in &self.entries {
            if entry.class != MappingClass::SharedIdentity {
                continue;
            }
            let pr = entry.mapping.physical_range;
            let start = pr.base.max(extent.base);
            let stop = (pr.base + pr.size).min(end);
            if start < stop {
                mappings
                    .try_reserve(1)
                    .map_err(|_| PlanError::OutOfMemory)?;
                mappings.push(slice_identity_piece(entry.mapping, start, stop - start));
            }
        }
        Ok(mappings)
    }

    /// 生成要落进一个 Isolated AS 的共享映射集合：`SharedCore` / `Device` 原样，
    /// `SharedIdentity` 按已发布私有 extent **切段**（不覆盖任何私有字节）。
    pub fn shared_mappings(&self) -> Result<Vec<Mapping>, PlanError> {
        let mut out = Vec::new();
        for entry in &self.entries {
            match entry.class {
                MappingClass::CoreRootOnly => {}
                MappingClass::SharedCore | MappingClass::Device => {
                    out.try_reserve(1).map_err(|_| PlanError::OutOfMemory)?;
                    out.push(entry.mapping);
                }
                MappingClass::SharedIdentity => {
                    let mut pieces = Vec::new();
                    pieces.try_reserve(1).map_err(|_| PlanError::OutOfMemory)?;
                    pieces.push(entry.mapping);
                    for exclusion in &self.exclusions {
                        let mut next = Vec::new();
                        for piece in pieces {
                            next.try_reserve(2).map_err(|_| PlanError::OutOfMemory)?;
                            subtract_identity(piece, *exclusion, &mut next);
                        }
                        pieces = next;
                    }
                    out.try_reserve(pieces.len())
                        .map_err(|_| PlanError::OutOfMemory)?;
                    out.extend(pieces);
                }
            }
        }
        Ok(out)
    }
}

/// 从 identity 映射 `piece`（VA==PA）里减去物理 `exclusion`，把剩余段 push 到
/// `out`。只处理真正相交的部分；不相交原样保留。
fn subtract_identity(piece: Mapping, exclusion: PhysicalRange, out: &mut Vec<Mapping>) {
    let Some(piece_end) = range_end(piece.physical_range.base, piece.physical_range.size) else {
        return;
    };
    let Some(ex_end) = range_end(exclusion.base, exclusion.size) else {
        return;
    };
    if exclusion.base >= piece_end || piece.physical_range.base >= ex_end {
        out.push(piece);
        return;
    }
    let cut_start = exclusion.base.max(piece.physical_range.base);
    let cut_end = ex_end.min(piece_end);
    if piece.physical_range.base < cut_start {
        out.push(slice_identity_piece(
            piece,
            piece.physical_range.base,
            cut_start - piece.physical_range.base,
        ));
    }
    if cut_end < piece_end {
        out.push(slice_identity_piece(piece, cut_end, piece_end - cut_end));
    }
}

/// identity 段 `[base, base+size)` 的映射切片（VA == PA）。
fn slice_identity_piece(piece: Mapping, base: usize, size: usize) -> Mapping {
    Mapping {
        virtual_range: VirtualRange { base, size },
        physical_range: PhysicalRange { base, size },
        permission: piece.permission,
    }
}

// ---------------------------------------------------------------------------
// 全局安装（boot 的 runtime builder 记录计划后交给 Core；Isolated AS 创建
// 从这里取共享映射）。
// ---------------------------------------------------------------------------

static PLAN: spin::Mutex<Option<KernelMappingPlan>> = spin::Mutex::new(None);

/// 安装计划（boot 的 runtime root 建立后调用一次；重复安装返回 `false`）。
pub fn install(plan: KernelMappingPlan) -> bool {
    let mut slot = PLAN.lock();
    if slot.is_some() {
        return false;
    }
    *slot = Some(plan);
    true
}

/// 取共享映射快照（没有安装计划 = 空；绝不伪造共享映射）。
pub fn shared_mappings() -> Result<Vec<Mapping>, super::address_space::MapError> {
    PLAN.lock()
        .as_ref()
        .map(KernelMappingPlan::shared_mappings)
        .unwrap_or_else(|| Ok(Vec::new()))
        .map_err(Into::into)
}

/// Keep root construction in the same transaction as backing publication and
/// release. A snapshot installed later could otherwise restore a stale alias.
#[cfg(any(
    feature = "vm-nommu",
    all(
        feature = "vm-mmu",
        any(target_arch = "riscv32", target_arch = "riscv64")
    )
))]
pub(crate) fn with_shared_mappings<T>(
    build: impl FnOnce(Vec<Mapping>) -> Result<T, super::address_space::MapError>,
) -> Result<T, super::address_space::MapError> {
    let slot = PLAN.lock();
    let shared = slot
        .as_ref()
        .map(KernelMappingPlan::shared_mappings)
        .unwrap_or_else(|| Ok(Vec::new()))?;
    let result = build(shared);
    drop(slot);
    result
}

/// **私有 backing 别名排除事务**：把一个刚分配、即将发布为组件私有的物理
/// extent 从**所有**已存在的 Isolated root 的 identity 映射里摘掉，并登记为
/// 后续 root 的排除项。
///
/// 调用方必须在把 backing 交付给组件**之前**调用；同时必须在 Core root 的
/// 视图里保留该 backing（loader / 拷贝 / 清理仍要访问）。
///
/// 语义：创建 B 之后，早先创建的 A **不能**再经它先前装上的 identity 映射
/// 看见 B 的 backing（快照式计划不够——必须对活着的 root 逐个摘除）。
pub fn publish_private_backing(
    extent: PhysicalRange,
) -> Result<(), super::address_space::MapError> {
    use super::address_space::{self, MapError};
    let granule = super::ALLOC_GRANULE;
    let mut slot = PLAN.lock();
    if let Some(plan) = slot.as_mut() {
        plan.exclude(extent, granule).map_err(|error| {
            if error == PlanError::OutOfMemory {
                MapError::OutOfMemory
            } else {
                MapError::Unaligned
            }
        })?;
    }
    // Lock order is PLAN → SPACES, shared by root construction and release.
    let result = address_space::exclude_identity_alias_from_live_spaces(extent);
    drop(slot);
    result
}

/// An explicit release has ended the private mapping's lifetime. Restore
/// shared aliases before returning its physical extent to the global allocator.
/// On error the caller must retain the backing; it is never safe to guess.
pub fn release_private_backing(
    extent: PhysicalRange,
) -> Result<(), super::address_space::MapError> {
    use super::address_space::{self, MapError};
    let mut slot = PLAN.lock();
    if let Some(plan) = slot.as_mut() {
        // A failed publication may never have installed an exclusion, or an
        // earlier teardown attempt already restored it. Both retain ownership
        // in the AS and can safely retry without inventing a second ledger.
        if !plan.exclusions.contains(&extent) {
            return Ok(());
        }
        let mappings = plan.restoration(extent).map_err(|error| {
            if error == PlanError::OutOfMemory {
                MapError::OutOfMemory
            } else {
                MapError::NotMapped
            }
        })?;
        for mapping in mappings {
            address_space::restore_identity_alias_to_live_spaces(mapping)?;
        }
        plan.exclusions.retain(|excluded| *excluded != extent);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::address_space::{MappingPermission, PhysicalRange, VirtualRange};
    use super::*;

    const PAGE: usize = 4096;

    fn mapping(va: usize, pa: usize, size: usize) -> Mapping {
        Mapping {
            virtual_range: VirtualRange { base: va, size },
            physical_range: PhysicalRange { base: pa, size },
            permission: MappingPermission::READ | MappingPermission::WRITE,
        }
    }

    #[test]
    fn restoration_uses_exact_extent_and_keeps_sibling_excluded() {
        let mut plan = KernelMappingPlan::empty();
        plan.add(
            MappingClass::SharedIdentity,
            mapping(0x8000, 0x8000, 4 * PAGE),
            PAGE,
        )
        .unwrap();
        let first = PhysicalRange {
            base: 0x8000,
            size: PAGE,
        };
        let second = PhysicalRange {
            base: 0xa000,
            size: PAGE,
        };
        plan.exclude(first, PAGE).unwrap();
        plan.exclude(second, PAGE).unwrap();
        assert!(
            plan.restoration(PhysicalRange {
                base: 0x8000,
                size: 2 * PAGE
            })
            .is_err()
        );
        assert_eq!(
            plan.restoration(first).unwrap(),
            alloc::vec![mapping(0x8000, 0x8000, PAGE)]
        );
        plan.exclusions.retain(|extent| *extent != first);
        assert!(!plan.is_excluded(first));
        assert!(plan.is_excluded(second));
        assert!(
            plan.shared_mappings()
                .unwrap()
                .iter()
                .any(|m| m.virtual_range.base == first.base)
        );
        assert!(!plan.shared_mappings().unwrap().iter().any(|m| {
            m.virtual_range.base <= second.base
                && second.base < m.virtual_range.base + m.virtual_range.size
        }));
    }

    #[test]
    fn restoration_cannot_reopen_overlapping_private_backing() {
        let mut plan = KernelMappingPlan::empty();
        let extent = PhysicalRange {
            base: 0x8000,
            size: 2 * PAGE,
        };
        plan.exclude(extent, PAGE).unwrap();
        plan.exclude(
            PhysicalRange {
                base: 0x9000,
                size: PAGE,
            },
            PAGE,
        )
        .unwrap();
        assert_eq!(
            plan.restoration(extent),
            Err(PlanError::PrivateAliasesShared)
        );
        assert!(plan.is_excluded(extent));
    }

    #[test]
    fn add_rejects_empty_unaligned_and_overlap() {
        let mut plan = KernelMappingPlan::empty();
        assert_eq!(
            plan.add(MappingClass::SharedCore, mapping(0x1000, 0x1000, 0), PAGE),
            Err(PlanError::Empty)
        );
        assert_eq!(
            plan.add(
                MappingClass::SharedCore,
                mapping(0x1001, 0x1000, PAGE),
                PAGE
            ),
            Err(PlanError::Unaligned)
        );
        plan.add(
            MappingClass::SharedCore,
            mapping(0x1000, 0x1000, PAGE),
            PAGE,
        )
        .unwrap();
        assert_eq!(
            plan.add(
                MappingClass::SharedCore,
                mapping(0x1000, 0x2000, PAGE),
                PAGE
            ),
            Err(PlanError::Overlap)
        );
    }

    #[test]
    fn shared_mappings_include_core_and_identity_but_not_root_only() {
        let mut plan = KernelMappingPlan::empty();
        plan.add(
            MappingClass::SharedCore,
            mapping(0xffff_0000, 0x8020_0000, 2 * PAGE),
            PAGE,
        )
        .unwrap();
        plan.add(
            MappingClass::SharedIdentity,
            mapping(0x8000_0000, 0x8000_0000, 8 * PAGE),
            PAGE,
        )
        .unwrap();
        plan.add(
            MappingClass::Device,
            mapping(0x0c00_0000, 0x0c00_0000, PAGE),
            PAGE,
        )
        .unwrap();
        plan.add(
            MappingClass::CoreRootOnly,
            mapping(0xffff_0000_0000, 0x8000_0000, PAGE),
            PAGE,
        )
        .unwrap();

        let shared = plan.shared_mappings().unwrap();
        assert_eq!(shared.len(), 3);
        assert_eq!(shared[0], mapping(0xffff_0000, 0x8020_0000, 2 * PAGE));
        assert_eq!(shared[1], mapping(0x8000_0000, 0x8000_0000, 8 * PAGE));
        assert_eq!(shared[2], mapping(0x0c00_0000, 0x0c00_0000, PAGE));
    }

    #[test]
    fn exclude_carves_private_extent_out_of_identity_ram_only() {
        let mut plan = KernelMappingPlan::empty();
        plan.add(
            MappingClass::SharedIdentity,
            mapping(0x8000_0000, 0x8000_0000, 4 * PAGE),
            PAGE,
        )
        .unwrap();
        plan.add(
            MappingClass::SharedCore,
            mapping(0xffff_0000, 0x9000_0000, 4 * PAGE),
            PAGE,
        )
        .unwrap();

        // 排除第二页（含 padding 的整段）。
        plan.exclude(
            PhysicalRange {
                base: 0x8000_1000,
                size: PAGE,
            },
            PAGE,
        )
        .unwrap();
        assert!(plan.is_excluded(PhysicalRange {
            base: 0x8000_1000,
            size: PAGE
        }));

        let shared = plan.shared_mappings().unwrap();
        // identity 段被切成两段；Core 固定映射原样。
        assert_eq!(shared.len(), 3);
        assert_eq!(shared[0], mapping(0x8000_0000, 0x8000_0000, PAGE));
        assert_eq!(shared[1], mapping(0x8000_2000, 0x8000_2000, 2 * PAGE));
        assert_eq!(shared[2], mapping(0xffff_0000, 0x9000_0000, 4 * PAGE));
    }

    #[test]
    fn exclude_rejects_overlap_with_non_identity_shared_mapping() {
        let mut plan = KernelMappingPlan::empty();
        plan.add(
            MappingClass::SharedCore,
            mapping(0xffff_0000, 0x8020_0000, PAGE),
            PAGE,
        )
        .unwrap();
        assert_eq!(
            plan.exclude(
                PhysicalRange {
                    base: 0x8020_0000,
                    size: PAGE
                },
                PAGE
            ),
            Err(PlanError::PrivateAliasesShared)
        );
    }

    #[test]
    fn same_plan_yields_same_va_to_pa_after_exclusion() {
        let mut plan = KernelMappingPlan::empty();
        plan.add(
            MappingClass::SharedIdentity,
            mapping(0x8000_0000, 0x8000_0000, 4 * PAGE),
            PAGE,
        )
        .unwrap();
        plan.exclude(
            PhysicalRange {
                base: 0x8000_2000,
                size: 2 * PAGE,
            },
            PAGE,
        )
        .unwrap();

        let a = plan.shared_mappings().unwrap();
        let b = plan.shared_mappings().unwrap();
        assert_eq!(a, b, "计划是确定性的：相同排除 → 相同共享映射");
        for m in &a {
            assert_eq!(
                m.virtual_range.base, m.physical_range.base,
                "identity 共享映射必须 same VA → same PA"
            );
        }
    }
}
