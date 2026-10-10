//! 按域装载：把一个已解析的 `.kcomp`（ELF32/ELF64 ET_REL）放进**某个实例的
//! 私有地址空间**，每段独立页范围 + 该段真正需要的权限。
//!
//! ```text
//! .kcomp 字节
//!   │  ElfObject::parse（复用 Core 私有 ELF API）
//!   │  import 白名单检查（诊断 / 只读查询 / panic / 私有 backing；面外拒绝）
//!   │  plan_sections：ALLOC 段 → 页对齐 VA + 权限（R+X / R / R+W），逐段独占页
//!   │  loader::apply_relocations（同一份 RISC-V 重定位实现，按域 base 重算）
//!   ▼
//! PlacedImage（段清单 + 入口 + backing region）
//!   │  map_into / map_mappings：逐段 VA → backing PA 落进实例 AS（失败回滚本次已落段）
//!   ▼
//! 实例 AS 里可经跨 AS trampoline 进入的镜像（由 `isolated_lifecycle` 调用）
//! ```
//!
//! # 与 KernelNative loader 的关系
//!
//! - **共享**：ELF 解析 / 重定位 / 符号解析 / `kcomp_abi` 值校验（直接复用
//!   `loader.rs` 的私有 API——同一份 arch 重定位实现，不复制算法）。
//! - **不复用**：KernelNative 的放段结果（共享内核 AS 的 VA 布局与 import 目标；
//!   `load.rs::create_isolated_native` 已在装载前拒绝 import 面之外的符号）。
//!   本模块**不调用** `loader::load_component`。[`PlacedImage::into_loaded_component`]
//!   把结果（lease + 入口）交给声明它的组件（`ComponentRecord.loaded`），落段用
//!   [`PlacedImage::mappings`] + [`map_mappings`] 完成——每次 instantiate 一份新
//!   backing。
//!
//! # 权限与页分离
//!
//! | 段 | 权限 | 依据 |
//! |---|---|---|
//! | `SHF_EXECINSTR` | `READ \| EXECUTE` | text：可执行、不可写 |
//! | `SHF_WRITE` | `READ \| WRITE` | data / bss |
//! | 其余 ALLOC | `READ` | rodata |
//!
//! **选择 pad 而不是 reject**：真实工具链输出里 `.text` / `.rodata` / `.data`
//! 常常首尾相接甚至同页，"段邻接"不是错误；若因为权限不同就拒绝，普通 `.kcomp`
//! 根本装不进来。因此每个 ALLOC 段拿到**自己的页对齐范围**（起点页对齐、长度
//! 向上取整到页），两个段不可能共享一个页——页级权限分离是结构保证，不是"但愿
//! 编译器不这样排"。代价是每段最多浪费不到一页。`validate_segments` 仍保留重叠
//! 拒绝作为纵深防御（规划器不可达，由单元测试直接喂重叠输入覆盖）。
//!
//! # 显式拒绝（绝不静默）
//!
//! import 白名单之外的具名 UNDEF 符号；段 VA 超出实例窗口 / 段间重叠 / 权限
//! 不可表达（空、W^X）/ 对齐非 2 的幂；入口不在任何可执行段内
//! （`EntryNotExecutable`）；没有私有 AS 能力的 profile（`map_into` 拒绝，绝不把
//! 恒等映射当 AS）。
//!
//! # 服务入口
//!
//! `kcomp_service_dispatch` 是**可选**的 image 级入口：定义了它的镜像会得到一个
//! **实例域内**的 dispatcher VA（[`PlacedImage::service_dispatch`]），供
//! `isolated_lifecycle` 经跨 AS trampoline 在私有 AS 里调用；与 create / destroy 同一
//! 纪律（必须落在一条 `READ|EXECUTE` 段内）。它**不是** import——组件仍然只能
//! 调用自己镜像内的代码。
//!
//! # 诚实边界
//!
//! 协作式、非对抗：S-mode 组件与 Core 同特权级，可以直接改 `satp` / 自己的页表。
//! 本模块证明的是"页表真的按段权限强制"，不是对抗隔离（真正的强制边界是 U-mode，
//! 未实现）。ASID 恒 0 + 全量 `sfence.vma`。

use super::elf::{ElfError, ElfObject, Section};
use super::loader::{self, LoadedComponent, LoaderError};
use crate::memory;
use crate::memory::address_space::{
    self, AddressSpaceHandle, MapError, Mapping, MappingPermission, PhysicalRange, VirtualRange,
};
use alloc::vec::Vec;
use arch::ComponentRelocationImpl;
use arch::component::RelocationBackend;

/// ELF 符号 kind：`STT_OBJECT`（与 loader 的私有常量同值，注释互指）。
const STT_OBJECT: u8 = 1;
/// ELF 符号 kind：`STT_FUNC`。
const STT_FUNC: u8 = 2;

/// 按域放段的页粒度：与 Sv32 / Sv39 backend 的 `VM_PAGE_SIZE` 一致。
///
/// 目标构建上用编译期断言钉住（host 构建没有真实 backend，见下方 `const _`）。
const PAGE: usize = 4096;

#[cfg(any(
    feature = "vm-nommu",
    all(
        feature = "vm-mmu",
        any(target_arch = "riscv32", target_arch = "riscv64")
    )
))]
const _: () = assert!(
    !<address_space::AddressSpaceImpl as arch::vm::AddressSpaceBackend>::PRIVATE_ADDRESS_SPACE
        || PAGE == <address_space::AddressSpaceImpl as arch::vm::AddressSpaceBackend>::GRANULE,
    "isolated placement page size must match the active address-space backend granule"
);

/// 私有域镜像的默认 VA 窗口（Core 策略：per-domain loader 的地址预算）。
///
/// 0x2000_0000 起 16 MiB：与 Core 镜像 VA、ArchTest 的控制页
/// / 栈（0x3000_0000 起）、KernelNative 的 shared-AS 区域都不重叠。
pub const ISOLATED_IMAGE_WINDOW: VirtualRange = VirtualRange {
    base: 0x2000_0000,
    size: 0x0100_0000,
};

/// 默认的每域装载基址（窗口起点，页对齐）。
pub const ISOLATED_IMAGE_BASE: usize = ISOLATED_IMAGE_WINDOW.base;

/// 按域装载的失败原因（全部显式；语义见模块文档）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolatedLoadError {
    /// ELF 解析 / 重定位 / 符号解析 / `kcomp_abi` 校验失败（复用 loader 词汇）。
    Loader(LoaderError),
    /// `e_machine` 与当前目标不一致。
    MachineMismatch,
    /// import 白名单之外的具名 UNDEF 符号在装载前显式拒绝。
    ImportsUnsupported,
    /// 段的 VA 区间（或装载基址）超出该实例允许的窗口。
    SegmentOutsideWindow,
    /// 两个段的 VA 区间重叠（页级权限分离的纵深防御检查；规划器按页分隔）。
    SegmentOverlap,
    /// 段需要的权限后端无法表达 / 本策略不接受（空权限集、W^X）。
    UnsupportedPermission,
    /// 段对齐不是 2 的幂。
    UnsupportedAlignment,
    /// 地址算术溢出（32 位目标的 `lui` 绝对寻址约束也走这里）。
    AddressOverflow,
    /// 入口地址不在任何可执行（R+X）段内。
    EntryNotExecutable,
    /// 该 profile 没有私有地址空间能力（NoMMU / 无 backend）：显式拒绝，
    /// 绝不把恒等翻译当成实例 AS。
    IsolationUnsupported,
    /// Core 地址空间管理器拒绝了某个段的映射（句柄 / 重叠 / 后端）。
    Map(MapError),
    /// 从组件仓库读取 artifact 失败（仓库未挂载 / 条目不存在 / 读失败）。
    /// 原样保留 `component::load` 的错误原因（不塌缩）。
    Artifact(super::load::ComponentLoadError),
}

/// 一个已放段的镜像区间（VA + 该段真正需要的权限）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlacedSegment {
    /// 在 ELF section 表里的下标（诊断 / 测试）。
    pub section: usize,
    /// 实例 AS 内的 VA 区间（页对齐、长度为页的整数倍）。
    pub virtual_range: VirtualRange,
    /// 相对 [`PlacedImage::base`] 的偏移（== `virtual_range.base - base`）。
    pub offset: usize,
    /// 该段真正需要的权限（`READ` / `READ|WRITE` / `READ|EXECUTE`）。
    pub permission: MappingPermission,
}

/// 一次按域放段的结果：段清单 + 入口 + 常驻 backing（**尚未落进任何 AS**）。
///
/// backing 是该实例的私有内存：其物理页只应经 [`PlacedImage::mapping`] 映射进
/// 该实例的 AS，不得映射 Core 段 / Core 堆 / 页表 / MMIO。
#[derive(Debug, PartialEq, Eq)]
pub struct PlacedImage {
    base: usize,
    create: usize,
    destroy: usize,
    service_dispatch: Option<usize>,
    runtime_init: Option<usize>,
    text_size: usize,
    abi: u64,
    segments: Vec<PlacedSegment>,
    region: memory::MemoryLease,
}

impl PlacedImage {
    /// 装载基址（窗口内、页对齐）。
    pub fn base(&self) -> usize {
        self.base
    }

    /// `kcomp_instance_create` 的实例 AS 内 VA（已验证落在 R+X 段内）。
    pub fn create(&self) -> usize {
        self.create
    }

    /// `kcomp_instance_destroy` 的实例 AS 内 VA（已验证落在 R+X 段内）。
    pub fn destroy(&self) -> usize {
        self.destroy
    }

    /// **可选**的 `kcomp_service_dispatch` 实例 AS 内 VA（已验证落在 R+X 段内）。
    /// `None` = 组件不提供 endpoint 服务（与 KernelNative image 同一语义）。
    pub fn service_dispatch(&self) -> Option<usize> {
        self.service_dispatch
    }

    /// 放段总跨度（字节；页对齐）。
    pub fn text_size(&self) -> usize {
        self.text_size
    }

    /// 已校验的 `kcomp_abi` 值（必等于 [`crate::component::containment::KCOMP_ABI`]）。
    pub fn abi(&self) -> u64 {
        self.abi
    }

    /// 段清单（按 ELF section 顺序；每段独占页范围）。
    pub fn segments(&self) -> &[PlacedSegment] {
        &self.segments
    }

    /// backing region 的物理基址（诊断 / 映射构造）。
    pub fn backing_base(&self) -> usize {
        self.region.base()
    }

    /// 某段在实例 AS 里的完整映射（VA → backing PA，权限 = 段权限）。
    pub fn mapping(&self, segment: &PlacedSegment) -> Mapping {
        Mapping {
            virtual_range: segment.virtual_range,
            physical_range: PhysicalRange {
                base: self.region.base() + segment.offset,
                size: segment.virtual_range.size,
            },
            permission: segment.permission,
        }
    }

    /// 全部段的映射清单（按 ELF section 顺序；页级权限分离）。
    ///
    /// 生命周期接线用它在 `into_loaded_component`（lease 转移给组件记录）之后仍然
    /// 能把同一份段规划落进实例 AS（规划不携带借用）。
    pub fn mappings(&self) -> Vec<Mapping> {
        self.segments
            .iter()
            .map(|segment| self.mapping(segment))
            .collect()
    }

    /// 转成声明组件所需的 [`LoadedComponent`]（lease 随本次转换归该组件记录）。
    ///
    /// Isolated 的 `create` / `destroy` / `service_dispatch` 都是**实例 AS 内**的
    /// VA（供 `component/isolated_lifecycle.rs` 经跨 AS trampoline 调用）。
    pub fn into_loaded_component(self) -> LoadedComponent {
        LoadedComponent {
            base: self.base,
            create: self.create,
            destroy: self.destroy,
            service_dispatch: self.service_dispatch,
            runtime_init: self.runtime_init,
            text_size: self.text_size,
            abi: self.abi,
            memory: Some(self.region),
        }
    }
}

/// 按默认窗口/基址放段（**不落 AS**；由 ArchTest / 生命周期接线消费）。
pub fn place(blob: &[u8]) -> Result<PlacedImage, IsolatedLoadError> {
    place_at(blob, ISOLATED_IMAGE_BASE, ISOLATED_IMAGE_WINDOW)
}

/// 从组件仓库读取 `<name>.kcomp` 并按默认窗口 / 基址放段。
///
/// 仓库读取复用 `component::load` 的同一份入口（`StoreNotMounted` / `NotFound`
/// / `ReadFailed` 原样保留为 [`IsolatedLoadError::Artifact`]）——本函数**不**登记
/// image、不触碰生命周期。
pub fn place_artifact(name: &[u8]) -> Result<PlacedImage, IsolatedLoadError> {
    let blob = super::load::read_artifact(name).map_err(IsolatedLoadError::Artifact)?;
    place(&blob)
}

/// 把已放段的镜像逐段落进实例 AS（失败即回滚本次已落段，不留半套镜像）。
///
/// 本函数**不**做激活准备（那是 `component::isolated::prepare` 的职责），
/// 也不触碰任何生命周期路径——调用方负责持有句柄与后续的 `prepare` / `enter`。
pub fn map_into(handle: AddressSpaceHandle, image: &PlacedImage) -> Result<(), IsolatedLoadError> {
    map_mappings(handle, &image.mappings())
}

/// 把一条**已记录的映射清单**落进实例 AS（失败即回滚本次已落映射）。
///
/// 与 [`map_into`] 同一机制，只是不携带 [`PlacedImage`]：生命周期接线在
/// `into_loaded_component`（lease 转移给组件记录）之后用它落段。没有私有 AS
/// 能力（NoMMU / 无 backend）时显式拒绝，绝不把恒等翻译当 AS。
pub fn map_mappings(
    handle: AddressSpaceHandle,
    mappings: &[Mapping],
) -> Result<(), IsolatedLoadError> {
    // 没有私有 AS 能力（NoMMU / 无 backend）就显式拒绝：绝不把恒等翻译当 AS。
    if !address_space::isolation_capable() {
        return Err(IsolatedLoadError::IsolationUnsupported);
    }
    let mut mapped: Vec<VirtualRange> = Vec::new();
    for mapping in mappings {
        match address_space::map(handle, *mapping) {
            Ok(()) => mapped.push(mapping.virtual_range),
            Err(error) => {
                // 半套镜像不留在实例 AS 里（best effort 回滚本次新落的段）。
                for range in mapped.iter().rev() {
                    let _ = address_space::unmap(handle, range);
                }
                return Err(IsolatedLoadError::Map(error));
            }
        }
    }
    Ok(())
}

/// Isolated 组件允许的 import 白名单（**唯一**的支持面）。
///
/// 支持**诊断 / 只读查询**、panic、私有 backing 与 endpoint 发布/发现/绑定/调用：
/// Isolated 保持 S-mode；普通 Core 入口不切 root，出站 call 由 Core 桥接。
/// **KernelNative 共享堆后端 `kcore_heap_alloc/dealloc`**、调度入口、组件创建、
/// 设备 / DMA / IRQ 获取等**仍然显式拒绝**——它们需要 Core 侧的所有权 / 生命周期
/// 裁决，或只是 KernelNative 受信部署形态的窄后端，不在本阶段的支持面内。
///
/// 任何不在白名单里的具名 UNDEF 符号都在装载前**显式拒绝**——绝不回退到裸
/// Core 地址，也绝不静默忽略。
pub const SUPPORTED_IMPORTS: &[&[u8]] = &[
    b"kcore_log_line",
    b"kcore_console_write_byte",
    b"kcore_now",
    b"kcore_timebase_hz",
    b"kcore_machine_boot_hart",
    b"kcore_machine_cpu_count",
    b"kcore_machine_has_hart",
    b"kcore_free_page_count",
    b"kcore_task_count",
    b"kcore_component_count",
    b"kcore_panic_escape",
    b"kcore_memory_acquire",
    b"kcore_memory_release",
    b"kcore_endpoint_publish",
    b"kcore_endpoint_lookup",
    b"kcore_endpoint_validate",
    b"kcore_endpoint_bind",
    b"kcore_endpoint_call",
    b"kcore_component_current",
    b"kcore_cpu_current",
    b"kcore_task_create",
    b"kcore_task_start",
    b"kcore_task_start_on",
    b"kcore_task_yield",
    b"kcore_task_exit",
    b"kcore_ipc_listen",
    b"kcore_ipc_grant",
    b"kcore_ipc_submit",
    b"kcore_ipc_receive",
    b"kcore_ipc_reply",
    b"kcore_ipc_collect",
    b"kcore_ipc_wait",
    b"kcore_ipc_cancel",
    b"kcore_ipc_close",
];

/// 该 UNDEF 符号是否是本阶段支持解析的 import（唯一判据）。
pub fn import_supported(name: &[u8]) -> bool {
    SUPPORTED_IMPORTS.contains(&name)
}

/// Isolated import 解析：只解析支持面内的符号，地址 = Core 导出地址（重定位时
/// 归一化到低别名；共享 identity RAM 在每个 Isolated AS 里都映射它，因此
/// `satp` 不切换、CALL 的 ±2 GiB 可达）。
fn resolve_import(name: &[u8]) -> Option<usize> {
    if !import_supported(name) {
        return None;
    }
    crate::component::export::resolve(name)
}

/// 解析 + 校验 + 按域放段（host-testable 的纯逻辑 + 一次 backing 分配）。
fn place_at(
    blob: &[u8],
    base: usize,
    window: VirtualRange,
) -> Result<PlacedImage, IsolatedLoadError> {
    place_at_domain(blob, base, window, false, true)
}

#[cfg(any(
    test,
    all(
        feature = "vm-mmu",
        feature = "supervisor",
        any(target_arch = "riscv32", target_arch = "riscv64")
    )
))]
pub(crate) fn place_for_lifecycle(
    blob: &[u8],
    sandboxed: bool,
) -> Result<PlacedImage, IsolatedLoadError> {
    // Production must register the image owner before a publication attempt
    // can make Drop unsafe. Standalone ArchTest placement keeps its old path.
    place_at_domain(
        blob,
        ISOLATED_IMAGE_BASE,
        ISOLATED_IMAGE_WINDOW,
        sandboxed,
        false,
    )
}
fn place_at_domain(
    blob: &[u8],
    base: usize,
    window: VirtualRange,
    sandboxed: bool,
    publish: bool,
) -> Result<PlacedImage, IsolatedLoadError> {
    let object = ElfObject::parse(blob).map_err(elf_error)?;
    if object.machine() != ComponentRelocationImpl::ELF_MACHINE {
        return Err(IsolatedLoadError::MachineMismatch);
    }
    if sandboxed {
        let table = object.symbol_table_index().map_err(elf_error)?;
        for index in 0..object.symbol_count(table).map_err(elf_error)? {
            let symbol = object.symbol(table, index).map_err(elf_error)?;
            if symbol.shndx != 0 {
                continue;
            }
            let name = object.symbol_name(table, symbol).map_err(elf_error)?;
            if !name.is_empty() && super::sandbox::resolve_import(name).is_none() {
                return Err(IsolatedLoadError::ImportsUnsupported);
            }
        }
    } else {
        check_supported_imports(&object)?;
    }

    let symbol_table = object.symbol_table_index().map_err(elf_error)?;
    let service_dispatch =
        loader::symbol_offset(&object, symbol_table, b"kcomp_service_dispatch", STT_FUNC)
            .map_err(IsolatedLoadError::Loader)?;
    let runtime_symbol =
        loader::symbol_offset(&object, symbol_table, b"kcomp_runtime_init", STT_FUNC)
            .map_err(IsolatedLoadError::Loader)?;
    let relocations = object.relocations().map_err(elf_error)?;

    let Placement {
        image_size,
        seg_place,
        segments,
    } = plan_sections(object.sections(), base, window)?;
    if image_size == 0 {
        return Err(IsolatedLoadError::Loader(LoaderError::NoTextSection));
    }

    let create_symbol =
        loader::symbol_offset(&object, symbol_table, b"kcomp_instance_create", STT_FUNC)
            .map_err(IsolatedLoadError::Loader)?
            .ok_or(IsolatedLoadError::Loader(LoaderError::MissingCreate))?;
    let destroy_symbol =
        loader::symbol_offset(&object, symbol_table, b"kcomp_instance_destroy", STT_FUNC)
            .map_err(IsolatedLoadError::Loader)?
            .ok_or(IsolatedLoadError::Loader(LoaderError::MissingDestroy))?;
    let abi_symbol = loader::symbol_offset(&object, symbol_table, b"kcomp_abi", STT_OBJECT)
        .map_err(IsolatedLoadError::Loader)?
        .ok_or(IsolatedLoadError::Loader(LoaderError::MissingAbi))?;

    let mut region = memory::alloc_region(image_size)
        .map_err(|_| IsolatedLoadError::Loader(LoaderError::OutOfMemory))?;
    let physical_base = region.base();
    // SAFETY: region 刚从 buddy heap 独占分配，长度 = image_size；identity/low-alias
    // 视图在目标由 boot 建立，host 测试里就是宿主指针——与 KernelNative loader 的
    // 既有放段方式相同（`loader.rs::load_component`）。
    let image = unsafe { core::slice::from_raw_parts_mut(physical_base as *mut u8, image_size) };
    for &(index, put) in &seg_place {
        let section = object.section(index).map_err(elf_error)?;
        let end = put
            .checked_add(section.size)
            .ok_or(IsolatedLoadError::AddressOverflow)?;
        let dst = image
            .get_mut(put..end)
            .ok_or(IsolatedLoadError::Loader(LoaderError::UnsupportedFormat))?;
        if section.is_nobits() {
            // BSS：无文件数据，放段 = 零填充。
            dst.fill(0);
        } else {
            let data = object.section_data(index).map_err(elf_error)?;
            dst.copy_from_slice(data);
        }
    }

    // 同一份 RISC-V 重定位实现，只换 base / 段偏移（按域重算，绝不复用 Native 结果）。
    loader::apply_relocations(
        &object,
        base,
        image,
        &seg_place,
        &relocations,
        if sandboxed {
            super::sandbox::resolve_import
        } else {
            resolve_import
        },
    )
    .map_err(IsolatedLoadError::Loader)?;

    let create = loader::resolve_symbol_address(&seg_place, base, create_symbol)
        .map_err(IsolatedLoadError::Loader)?;
    let destroy = loader::resolve_symbol_address(&seg_place, base, destroy_symbol)
        .map_err(IsolatedLoadError::Loader)?;
    let abi =
        loader::read_abi(image, base, &seg_place, abi_symbol).map_err(IsolatedLoadError::Loader)?;
    ensure_executable_entry(&segments, create)?;
    ensure_executable_entry(&segments, destroy)?;
    // 可选服务入口：同一纪律——必须落在一条 R+X 段内。
    let service_dispatch = match service_dispatch {
        Some(symbol) => {
            let address = loader::resolve_symbol_address(&seg_place, base, symbol)
                .map_err(IsolatedLoadError::Loader)?;
            ensure_executable_entry(&segments, address)?;
            Some(address)
        }
        None => None,
    };

    let runtime_init = match runtime_symbol {
        Some(symbol) => {
            let address = loader::resolve_symbol_address(&seg_place, base, symbol)
                .map_err(IsolatedLoadError::Loader)?;
            ensure_executable_entry(&segments, address)?;
            Some(address)
        }
        None => None,
    };
    // All image validation precedes publication: rejected images can be freed
    // without leaving alias exclusions. Published images remain resident even
    // if publication or a later lifecycle step fails (phase 1 contract).
    if publish {
        region.retain_on_drop();
        crate::memory::kernel_mappings::publish_private_backing(region.region())
            .map_err(IsolatedLoadError::Map)?;
    }
    let segments = if sandboxed {
        segments
            .into_iter()
            .map(|mut segment| {
                segment.permission |= MappingPermission::USER;
                segment
            })
            .collect()
    } else {
        segments
    };
    Ok(PlacedImage {
        base,
        create,
        destroy,
        service_dispatch,
        runtime_init,
        text_size: image_size,
        abi,
        segments,
        region,
    })
}

/// 扫描符号表：每个具名 UNDEF 符号都必须命中 [`SUPPORTED_IMPORTS`]。
///
/// 索引 0 是 ELF 规定的 NULL 符号（UNDEF、无名），不算 import。
fn check_supported_imports(object: &ElfObject<'_>) -> Result<(), IsolatedLoadError> {
    let symbol_table = object.symbol_table_index().map_err(elf_error)?;
    let count = object.symbol_count(symbol_table).map_err(elf_error)?;
    for index in 1..count {
        let symbol = object.symbol(symbol_table, index).map_err(elf_error)?;
        if symbol.shndx != 0 {
            continue;
        }
        let name = object
            .symbol_name(symbol_table, symbol)
            .map_err(elf_error)?;
        if !name.is_empty() && !import_supported(name) {
            return Err(IsolatedLoadError::ImportsUnsupported);
        }
    }
    Ok(())
}

/// 依 ELF 段顺序规划放置：每段起点页对齐、长度向上取整到页（**独占页范围**），
/// 并检查窗口 / 权限 / 重叠。
struct Placement {
    /// 放段总跨度（字节）。
    image_size: usize,
    /// `(shndx, 相对 base 的放段偏移)`：loader 的重定位 / 符号解析坐标。
    seg_place: Vec<(usize, usize)>,
    /// 需要落进实例 AS 的段（零长段只占 `seg_place`，不产生映射）。
    segments: Vec<PlacedSegment>,
}

fn plan_sections(
    sections: &[Section],
    base: usize,
    window: VirtualRange,
) -> Result<Placement, IsolatedLoadError> {
    if !is_page_aligned(base) {
        return Err(IsolatedLoadError::UnsupportedAlignment);
    }
    let mut cursor = 0usize;
    let mut seg_place = Vec::new();
    let mut segments = Vec::new();
    for (index, section) in sections.iter().enumerate() {
        if !section.is_alloc_content() {
            continue;
        }
        // 段对齐 ≤ 页时页对齐起点即满足；> 页时按绝对 VA 对齐（偏移仍是页整数倍）。
        let align = effective_alignment(section.align)?;
        let start = base
            .checked_add(cursor)
            .ok_or(IsolatedLoadError::AddressOverflow)?;
        let va = align_up(start, align)?;
        let offset = va
            .checked_sub(base)
            .ok_or(IsolatedLoadError::AddressOverflow)?;
        seg_place.push((index, offset));
        let size = align_up(section.size, PAGE)?;
        if !range_contains(&window, va, size) {
            return Err(IsolatedLoadError::SegmentOutsideWindow);
        }
        if size != 0 {
            segments.push(PlacedSegment {
                section: index,
                virtual_range: VirtualRange { base: va, size },
                offset,
                permission: segment_permission(*section)?,
            });
        }
        cursor = offset
            .checked_add(size)
            .ok_or(IsolatedLoadError::AddressOverflow)?;
    }
    validate_segments(&segments, &window)?;
    Ok(Placement {
        image_size: cursor,
        seg_place,
        segments,
    })
}

/// 段权限：text = R+X、data/bss = R+W、rodata = R。
///
/// W^X 显式拒绝（后端能编码 RWX，但页级权限分离的整个意义就是不让它出现）；
/// 顺带拒绝后端无法表达的权限集（空、W 而无 R）——它不能从 ELF flags 产生，
/// 是纵深防御，规则与 `arch` 的 `TryFrom<MappingPermission> for PteFlags` 对齐。
fn segment_permission(section: Section) -> Result<MappingPermission, IsolatedLoadError> {
    let permission = match (section.is_write(), section.is_exec()) {
        // W^X：不可接受。
        (true, true) => return Err(IsolatedLoadError::UnsupportedPermission),
        (false, true) => MappingPermission::READ | MappingPermission::EXECUTE,
        (true, false) => MappingPermission::READ | MappingPermission::WRITE,
        (false, false) => MappingPermission::READ,
    };
    if !permission
        .intersects(MappingPermission::READ | MappingPermission::WRITE | MappingPermission::EXECUTE)
        || (permission.contains(MappingPermission::WRITE)
            && !permission.contains(MappingPermission::READ))
    {
        return Err(IsolatedLoadError::UnsupportedPermission);
    }
    Ok(permission)
}

/// 规划结果校验：段必须在窗口内、两两不重叠（页级权限分离的纵深防御）。
fn validate_segments(
    segments: &[PlacedSegment],
    window: &VirtualRange,
) -> Result<(), IsolatedLoadError> {
    for (index, segment) in segments.iter().enumerate() {
        let range = segment.virtual_range;
        if !range_contains(window, range.base, range.size) {
            return Err(IsolatedLoadError::SegmentOutsideWindow);
        }
        for other in &segments[index + 1..] {
            if ranges_overlap(&range, &other.virtual_range) {
                return Err(IsolatedLoadError::SegmentOverlap);
            }
        }
    }
    Ok(())
}

/// 入口必须在一条 `READ|EXECUTE` 段内（与 `prepare_transition` 同一判据）。
fn ensure_executable_entry(
    segments: &[PlacedSegment],
    address: usize,
) -> Result<(), IsolatedLoadError> {
    let found = segments.iter().find(|segment| {
        address >= segment.virtual_range.base
            && address < segment.virtual_range.base + segment.virtual_range.size
    });
    match found {
        Some(segment) if segment.permission.contains(MappingPermission::EXECUTE) => Ok(()),
        _ => Err(IsolatedLoadError::EntryNotExecutable),
    }
}

fn is_page_aligned(value: usize) -> bool {
    value.is_multiple_of(PAGE)
}

/// 有效段对齐 = `max(页粒度, sh_addralign)`；`sh_addralign` 非 2 的幂时显式拒绝
/// （绝不静默按更小对齐放置，与 loader 的 `align_up` 同规则）。
fn effective_alignment(declared: usize) -> Result<usize, IsolatedLoadError> {
    if declared > 1 && !declared.is_power_of_two() {
        return Err(IsolatedLoadError::UnsupportedAlignment);
    }
    Ok(declared.max(PAGE))
}

/// 向上对齐到 `align`（2 的幂；`0` / `1` 视为不对齐）。非 2 的幂显式拒绝。
fn align_up(value: usize, align: usize) -> Result<usize, IsolatedLoadError> {
    if align <= 1 {
        return Ok(value);
    }
    if !align.is_power_of_two() {
        return Err(IsolatedLoadError::UnsupportedAlignment);
    }
    let mask = align - 1;
    value
        .checked_add(mask)
        .map(|aligned| aligned & !mask)
        .ok_or(IsolatedLoadError::AddressOverflow)
}

/// `[base, base + size)`（size 可为 0：只检查起点）是否被 `range` 完整覆盖。
fn range_contains(range: &VirtualRange, base: usize, size: usize) -> bool {
    let Some(end) = base.checked_add(size) else {
        return false;
    };
    let Some(range_end) = range.base.checked_add(range.size) else {
        return false;
    };
    range.base <= base && end <= range_end
}

fn ranges_overlap(a: &VirtualRange, b: &VirtualRange) -> bool {
    a.base < b.base + b.size && b.base < a.base + a.size
}

fn elf_error(error: ElfError) -> IsolatedLoadError {
    IsolatedLoadError::Loader(LoaderError::from(error))
}

// 这些用例需要 make test-host 准备的真实 `.kcomp` fixture（kcomp_isolated /
// kcomp_smoke）；`test-fixtures` 显式启用工件测试。
#[cfg(all(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::component::containment::KCOMP_ABI;
    use crate::memory::test_support;

    const ISOLATED_KCOMP: &[u8] = include_bytes!(concat!(
        env!("KALEIDOS_TEST_FIXTURES"),
        "/kcomp_isolated.kcomp"
    ));
    const SVC_KCOMP: &[u8] = include_bytes!(concat!(
        env!("KALEIDOS_TEST_FIXTURES"),
        "/kcomp_isolated_svc.kcomp"
    ));
    /// 支持面之外的 import 夹具（`kcore_heap_alloc`）。
    const UNSUPPORTED_KCOMP: &[u8] = include_bytes!(concat!(
        env!("KALEIDOS_TEST_FIXTURES"),
        "/kcomp_isolated_unsupported.kcomp"
    ));
    /// 放段失败夹具（17 MiB `.bss` 超出实例窗口）。
    const BAD_KCOMP: &[u8] = include_bytes!(concat!(
        env!("KALEIDOS_TEST_FIXTURES"),
        "/kcomp_isolated_bad.kcomp"
    ));

    fn in_range(segment: &PlacedSegment, address: usize) -> bool {
        address >= segment.virtual_range.base
            && address < segment.virtual_range.base + segment.virtual_range.size
    }

    /// 真实夹具：段按权限分类放段，每段独占页范围（页级权限分离）。
    #[test]
    fn places_fixture_with_page_separated_permissions() {
        // 放段分配并发布常驻 backing：GUARD 覆盖整个物理堆测试。
        let _guard = test_support::GUARD.lock();
        test_support::ensure_init();
        let image = place(ISOLATED_KCOMP).expect("kcomp_isolated must place");
        assert_eq!(image.base(), ISOLATED_IMAGE_BASE);
        assert_eq!(image.abi(), KCOMP_ABI, "kcomp_abi 必须逐位等于 Core 指纹");
        assert_eq!(image.text_size() % PAGE, 0);
        assert!(image.text_size() >= PAGE);

        // 段区间：页对齐、页整数倍、窗口内。
        let mut ranges: Vec<VirtualRange> = image
            .segments()
            .iter()
            .map(|segment| segment.virtual_range)
            .collect();
        ranges.sort_by_key(|range| range.base);
        for range in &ranges {
            assert!(is_page_aligned(range.base), "段 VA 必须页对齐");
            assert_eq!(range.size % PAGE, 0, "段长度必须是页的整数倍");
            assert!(
                range_contains(&ISOLATED_IMAGE_WINDOW, range.base, range.size),
                "段必须落在实例窗口内"
            );
        }
        // 页级权限分离：按序相邻的段不共享页（段区间不相交即页不相交）。
        for pair in ranges.windows(2) {
            assert!(
                pair[0].base + pair[0].size <= pair[1].base,
                "不同段不得共享页（padding 决定）"
            );
        }

        // 三类权限都存在，且没有 W^X 段。
        let has = |permission: MappingPermission| {
            image
                .segments()
                .iter()
                .any(|segment| segment.permission == permission)
        };
        assert!(has(MappingPermission::READ | MappingPermission::EXECUTE));
        assert!(has(MappingPermission::READ));
        assert!(has(MappingPermission::READ | MappingPermission::WRITE));
        assert!(
            image.segments().iter().all(|segment| !segment
                .permission
                .contains(MappingPermission::EXECUTE)
                || !segment.permission.contains(MappingPermission::WRITE)),
            "W^X 段必须被拒绝"
        );

        // 入口在 R+X 段内。
        for entry in [image.create(), image.destroy()] {
            let segment = image
                .segments()
                .iter()
                .find(|segment| in_range(segment, entry))
                .expect("入口必须落在某个段内");
            assert!(segment.permission.contains(MappingPermission::EXECUTE));
            assert!(!segment.permission.contains(MappingPermission::WRITE));
        }

        // 映射：VA → backing PA 一一对应，PA 页对齐。
        for segment in image.segments() {
            let mapping = image.mapping(segment);
            assert_eq!(mapping.virtual_range, segment.virtual_range);
            assert_eq!(
                mapping.physical_range.base,
                image.backing_base() + segment.offset
            );
            assert!(is_page_aligned(mapping.physical_range.base));
            assert_eq!(mapping.physical_range.size, segment.virtual_range.size);
            assert_eq!(mapping.permission, segment.permission);
        }
    }

    /// 生命周期接线的转换：`PlacedImage` → 组件记录登记的
    /// `LoadedComponent`（lease 随转换转移；create / destroy 是实例内 VA；
    /// Isolated image 没有 `service_dispatch`）。
    #[test]
    fn into_loaded_component_preserves_the_placement_truth() {
        let _guard = test_support::GUARD.lock();
        test_support::ensure_init();
        let image = place(ISOLATED_KCOMP).expect("place");
        let base = image.base();
        let create = image.create();
        let destroy = image.destroy();
        let text_size = image.text_size();
        let abi = image.abi();

        let loaded = image.into_loaded_component();

        assert_eq!(loaded.base, base);
        assert_eq!(loaded.create, create);
        assert_eq!(loaded.destroy, destroy);
        assert_eq!(loaded.service_dispatch, None, "Isolated image 无服务入口");
        assert_eq!(loaded.text_size, text_size);
        assert_eq!(loaded.abi, abi);
        assert!(loaded.memory.is_some(), "lease 必须随转换转移给组件记录");
    }

    #[test]
    fn unpublished_lifecycle_image_drop_returns_its_backing() {
        let _guard = test_support::GUARD.lock();
        test_support::ensure_init();
        let before = memory::free_block_counts();
        let image = place_for_lifecycle(ISOLATED_KCOMP, false).unwrap();
        assert_ne!(memory::free_block_counts(), before);
        drop(image.into_loaded_component());
        assert_eq!(memory::free_block_counts(), before);
    }

    #[test]
    fn published_image_remains_resident_after_owner_drop() {
        let _guard = test_support::GUARD.lock();
        test_support::ensure_init();
        let image = place(ISOLATED_KCOMP).unwrap();
        let region = image.region.region();
        let occupied = memory::free_block_counts();
        drop(image.into_loaded_component());
        assert_eq!(memory::free_block_counts(), occupied);
        // Host tests have no installed shared plan or live AS; cleanup is safe.
        memory::free_region_raw(region.base, region.size).unwrap();
    }

    /// 放段是确定性的；换基址只平移 VA（重定位按域重算），偏移不变。
    #[test]
    fn place_is_deterministic_and_per_base() {
        let _guard = test_support::GUARD.lock();
        test_support::ensure_init();
        let first = place(ISOLATED_KCOMP).expect("first place");
        let second = place(ISOLATED_KCOMP).expect("second place");
        assert_eq!(first.segments(), second.segments());
        assert_eq!(first.create(), second.create());
        assert_eq!(first.destroy(), second.destroy());
        assert_eq!(first.text_size(), second.text_size());

        const SHIFT: usize = 0x0020_0000;
        let shifted = place_at(
            ISOLATED_KCOMP,
            ISOLATED_IMAGE_BASE + SHIFT,
            ISOLATED_IMAGE_WINDOW,
        )
        .expect("shifted place");
        assert_eq!(shifted.segments().len(), first.segments().len());
        for (original, moved) in first.segments().iter().zip(shifted.segments()) {
            assert_eq!(original.offset, moved.offset);
            assert_eq!(
                moved.virtual_range.base,
                original.virtual_range.base + SHIFT
            );
            assert_eq!(moved.permission, original.permission);
        }
        assert_eq!(shifted.create(), first.create() + SHIFT);
        assert_eq!(shifted.destroy(), first.destroy() + SHIFT);
    }

    /// 支持面之外的 import 在装载前拒绝。
    #[test]
    fn rejects_unsupported_imports() {
        let _guard = test_support::GUARD.lock();
        test_support::ensure_init();
        assert_eq!(
            place(UNSUPPORTED_KCOMP),
            Err(IsolatedLoadError::ImportsUnsupported),
            "kcore_heap_alloc 必须被按域装载拒绝"
        );
    }

    /// 支持面过滤是纯逻辑（host-testable）；真实 `.kcomp` 的**端到端直接调用**
    /// 由 QEMU ArchTest `isolated-direct-imports`（RV64 + RV32）证明——host 上
    /// Core 导出地址是宿主指针，CALL 的 ±2 GiB 重定位范围不成立，因此这里只钉
    /// 过滤面（绝不把宿主地址当成可解析目标）。
    #[test]
    fn supported_import_surface_is_narrow_and_explicit() {
        for name in [
            b"kcore_log_line".as_slice(),
            b"kcore_console_write_byte",
            b"kcore_now",
            b"kcore_timebase_hz",
            b"kcore_machine_boot_hart",
            b"kcore_machine_cpu_count",
            b"kcore_machine_has_hart",
            b"kcore_free_page_count",
            b"kcore_task_count",
            b"kcore_component_count",
            b"kcore_panic_escape",
            b"kcore_memory_acquire",
            b"kcore_memory_release",
            b"kcore_endpoint_publish",
            b"kcore_endpoint_lookup",
            b"kcore_endpoint_validate",
            b"kcore_endpoint_bind",
            b"kcore_endpoint_call",
            b"kcore_component_current",
            b"kcore_cpu_current",
            b"kcore_task_create",
            b"kcore_task_start",
            b"kcore_task_start_on",
            b"kcore_task_yield",
            b"kcore_task_exit",
            b"kcore_ipc_listen",
            b"kcore_ipc_grant",
            b"kcore_ipc_submit",
            b"kcore_ipc_receive",
            b"kcore_ipc_reply",
            b"kcore_ipc_collect",
            b"kcore_ipc_wait",
            b"kcore_ipc_cancel",
            b"kcore_ipc_close",
        ] {
            assert!(import_supported(name), "{name:?} must be supported");
        }
        for name in [
            b"kcore_heap_alloc".as_slice(),
            b"kcore_heap_dealloc",
            b"kcore_sched_run",
            b"kcore_component_create",
            b"kcore_device_claim",
            b"kcore_dma_alloc",
            b"kcore_irq_register",
        ] {
            assert!(!import_supported(name), "{name:?} must be rejected");
        }
    }

    /// 服务入口必须落在 **R+X** 段内：把 dispatcher 的符号段改成非可执行段 →
    /// 显式拒绝（`EntryNotExecutable`），绝不把数据页当可调用入口。
    #[test]
    fn rejects_service_dispatch_outside_an_executable_segment() {
        // 放段会分配 backing（即使随后拒绝）：与其它 buddy heap 用例串行。
        let _guard = test_support::GUARD.lock();
        test_support::ensure_init();
        let mut patched = SVC_KCOMP.to_vec();
        let object = ElfObject::parse(&patched).expect("parse fixture");
        let data = object
            .sections()
            .iter()
            .position(|section| section.is_alloc() && !section.is_exec())
            .expect("fixture must have a non-executable ALLOC section");
        patch_symbol_shndx(&mut patched, b"kcomp_service_dispatch", data as u16)
            .expect("fixture defines the dispatcher symbol");
        let free_before = memory::free_block_counts();
        assert_eq!(place(&patched), Err(IsolatedLoadError::EntryNotExecutable));
        assert_eq!(
            memory::free_block_counts(),
            free_before,
            "rejected image backing is reusable"
        );
    }

    #[test]
    fn rejects_runtime_init_outside_an_executable_segment_without_publishing() {
        let _guard = test_support::GUARD.lock();
        test_support::ensure_init();
        let mut patched = SVC_KCOMP.to_vec();
        let data = ElfObject::parse(&patched)
            .unwrap()
            .sections()
            .iter()
            .position(|section| section.is_alloc() && !section.is_exec())
            .unwrap();
        patch_symbol_shndx(&mut patched, b"kcomp_service_dispatch", data as u16).unwrap();
        // This import-free fixture supplies a function symbol we can rename to
        // exercise the optional runtime entry's post-relocation validation.
        let name = b"kcomp_service_dispatch\0";
        let at = patched
            .windows(name.len())
            .position(|bytes| bytes == name)
            .unwrap();
        patched[at..at + name.len()].fill(0);
        patched[at..at + b"kcomp_runtime_init".len()].copy_from_slice(b"kcomp_runtime_init");
        let free_before = memory::free_block_counts();
        assert_eq!(place(&patched), Err(IsolatedLoadError::EntryNotExecutable));
        assert_eq!(memory::free_block_counts(), free_before);
    }

    /// `kcomp_service_dispatch` 夹具：真实定义了该入口的 `.kcomp` 按域放段
    /// 成功，且 dispatcher 的解析结果随 `into_loaded_component` 进入组件记录。
    #[test]
    fn places_service_dispatch_fixture_and_preserves_the_entry() {
        let _guard = test_support::GUARD.lock();
        test_support::ensure_init();
        let image = place(SVC_KCOMP).expect("kcomp_isolated_svc must place");
        let dispatch = image
            .service_dispatch()
            .expect("fixture defines kcomp_service_dispatch");
        let segment = image
            .segments()
            .iter()
            .find(|segment| in_range(segment, dispatch))
            .expect("dispatcher must be inside a segment");
        assert!(segment.permission.contains(MappingPermission::EXECUTE));
        assert!(!segment.permission.contains(MappingPermission::WRITE));

        let loaded = image.into_loaded_component();
        assert_eq!(loaded.service_dispatch, Some(dispatch));
    }

    /// `kcomp_abi` 值被改 → AbiMismatch（与 KernelNative loader 同一校验）。
    #[test]
    fn rejects_abi_mismatch() {
        // 放段会在读 ABI 之前分配 backing（即使随后拒绝）：与其它 buddy heap
        // 用例串行（见 memory::test_support）。
        let _guard = test_support::GUARD.lock();
        test_support::ensure_init();

        let object = ElfObject::parse(ISOLATED_KCOMP).expect("parse fixture");
        let symtab = object.symbol_table_index().expect("symtab");
        let (shndx, value) = loader::symbol_offset(&object, symtab, b"kcomp_abi", STT_OBJECT)
            .expect("symbol lookup")
            .expect("abi symbol");
        let section = object.section(shndx).expect("abi section");
        let file_offset = section.offset + value;

        let mut patched = ISOLATED_KCOMP.to_vec();
        patched[file_offset..file_offset + 8].copy_from_slice(&0xDEAD_BEEFu64.to_le_bytes());
        let free_before = memory::free_block_counts();
        assert_eq!(
            place(&patched),
            Err(IsolatedLoadError::Loader(LoaderError::AbiMismatch))
        );
        assert_eq!(
            memory::free_block_counts(),
            free_before,
            "bad ABI must not publish its backing"
        );
    }

    #[test]
    fn rejects_machine_mismatch() {
        let mut patched = ISOLATED_KCOMP.to_vec();
        patched[18..20].copy_from_slice(&0x3Eu16.to_le_bytes());
        assert_eq!(place(&patched), Err(IsolatedLoadError::MachineMismatch));
    }

    /// **放段失败夹具**：一份通过 packer 契约校验与 import 白名单、
    /// 但段超出实例窗口的真实 `.kcomp` → `SegmentOutsideWindow`。ArchTest
    /// `isolated-load-reject` 用同一份夹具证明生产创建入口把它拒绝成
    /// `IsolatedPlacementFailed`（声明 / AS 创建之前）。
    #[test]
    fn rejects_the_oversized_fixture_before_any_allocation() {
        let _guard = test_support::GUARD.lock();
        test_support::ensure_init();
        assert_eq!(
            place(BAD_KCOMP),
            Err(IsolatedLoadError::SegmentOutsideWindow),
            "kcomp_isolated_bad 的超大 .bss 必须在放段阶段被拒绝"
        );
    }

    /// 窗口放不下第二段 → 显式拒绝（不是截断、不是跳过）。
    #[test]
    fn rejects_section_outside_window() {
        let window = VirtualRange {
            base: ISOLATED_IMAGE_BASE,
            size: PAGE,
        };
        assert_eq!(
            place_at(ISOLATED_KCOMP, ISOLATED_IMAGE_BASE, window),
            Err(IsolatedLoadError::SegmentOutsideWindow)
        );
    }

    /// `sh_addralign` 非 2 的幂 → 拒绝（绝不静默按页对齐放置）。
    #[test]
    fn rejects_non_power_of_two_section_alignment() {
        let mut patched = ISOLATED_KCOMP.to_vec();
        assert_eq!(patched[4], 2, "host fixture 预期是 ELF64");
        patch_elf64_section_alignment(&mut patched, 1, 3);
        assert_eq!(
            place(&patched),
            Err(IsolatedLoadError::UnsupportedAlignment)
        );
    }

    /// 段权限映射：唯一来源是 ELF flags；W^X 显式拒绝。
    #[test]
    fn segment_permission_maps_flags_and_rejects_wx() {
        const ALLOC: u64 = 0x2;
        const WRITE: u64 = 0x1;
        const EXEC: u64 = 0x4;

        assert_eq!(
            segment_permission(synthetic_section(ALLOC | EXEC)),
            Ok(MappingPermission::READ | MappingPermission::EXECUTE)
        );
        assert_eq!(
            segment_permission(synthetic_section(ALLOC | WRITE)),
            Ok(MappingPermission::READ | MappingPermission::WRITE)
        );
        assert_eq!(
            segment_permission(synthetic_section(ALLOC)),
            Ok(MappingPermission::READ)
        );
        assert_eq!(
            segment_permission(synthetic_section(ALLOC | WRITE | EXEC)),
            Err(IsolatedLoadError::UnsupportedPermission)
        );
    }

    /// 重叠（尤其是不同权限）→ `SegmentOverlap`；出窗 → `SegmentOutsideWindow`。
    ///
    /// 生产规划器按页分隔使重叠不可达，这里直接喂重叠输入锁住纵深防御。
    #[test]
    fn validate_segments_rejects_overlap_and_outside_window() {
        let exec = PlacedSegment {
            section: 0,
            virtual_range: VirtualRange {
                base: ISOLATED_IMAGE_BASE,
                size: PAGE,
            },
            offset: 0,
            permission: MappingPermission::READ | MappingPermission::EXECUTE,
        };
        let data = PlacedSegment {
            section: 1,
            virtual_range: VirtualRange {
                base: ISOLATED_IMAGE_BASE,
                size: PAGE,
            },
            offset: 0,
            permission: MappingPermission::READ | MappingPermission::WRITE,
        };
        assert_eq!(
            validate_segments(&[exec, data], &ISOLATED_IMAGE_WINDOW),
            Err(IsolatedLoadError::SegmentOverlap),
            "不同权限的段重叠必须显式拒绝"
        );

        let adjacent = PlacedSegment {
            section: 2,
            virtual_range: VirtualRange {
                base: ISOLATED_IMAGE_BASE + PAGE,
                size: PAGE,
            },
            offset: PAGE,
            permission: MappingPermission::READ | MappingPermission::WRITE,
        };
        assert_eq!(
            validate_segments(&[exec, adjacent], &ISOLATED_IMAGE_WINDOW),
            Ok(())
        );

        let outside = PlacedSegment {
            section: 3,
            virtual_range: VirtualRange {
                base: ISOLATED_IMAGE_WINDOW.base + ISOLATED_IMAGE_WINDOW.size,
                size: PAGE,
            },
            offset: ISOLATED_IMAGE_WINDOW.size,
            permission: MappingPermission::READ,
        };
        assert_eq!(
            validate_segments(&[outside], &ISOLATED_IMAGE_WINDOW),
            Err(IsolatedLoadError::SegmentOutsideWindow)
        );
    }

    /// 没有私有 AS 能力（host / NoMMU）→ `map_into` 显式拒绝，绝不静默落映射。
    #[test]
    fn map_into_rejects_profiles_without_private_address_space() {
        // 放段操作访问物理堆：GUARD 必须覆盖整个用例。
        let _guard = test_support::GUARD.lock();
        test_support::ensure_init();
        let image = place(ISOLATED_KCOMP).expect("kcomp_isolated must place");
        let handle = AddressSpaceHandle::from_raw(1, 1);
        assert_eq!(
            map_into(handle, &image),
            Err(IsolatedLoadError::IsolationUnsupported)
        );
    }

    fn synthetic_section(flags: u64) -> Section {
        Section {
            ty: 1, // SHT_PROGBITS
            offset: 0,
            size: 0x20,
            link: 0,
            flags,
            info: 0,
            align: 8,
        }
    }

    /// ELF64 shdr 的 `sh_addralign` 在 +48（e_shoff 在 +40）。
    fn patch_elf64_section_alignment(blob: &mut [u8], index: usize, align: u64) {
        let shoff = u64::from_le_bytes(blob[40..48].try_into().unwrap()) as usize;
        let at = shoff + index * 64 + 48;
        blob[at..at + 8].copy_from_slice(&align.to_le_bytes());
    }

    /// 把某个已定义符号的 `st_shndx` 改成另一个 section（只改符号表，不动段内容 /
    /// 重定位）：用来喂"入口不在可执行段内"这类纵深防御检查。
    ///
    /// 只支持 host fixture 的 ELF64（`e_shentsize` / 符号项大小都是 64/24）。
    fn patch_symbol_shndx(blob: &mut [u8], name: &[u8], shndx: u16) -> Option<()> {
        assert_eq!(blob[4], 2, "host fixture 预期是 ELF64");
        let object = ElfObject::parse(blob).ok()?;
        let symtab = object.symbol_table_index().ok()?;
        let symtab_offset = object.section(symtab).ok()?.offset;
        for index in 0..object.symbol_count(symtab).ok()? {
            let symbol = object.symbol(symtab, index).ok()?;
            if object.symbol_name(symtab, symbol).ok()? != name {
                continue;
            }
            // Elf64_Sym：st_name(+0) st_info(+4) st_other(+5) st_shndx(+6)。
            let at = symtab_offset + index * 24 + 6;
            blob[at..at + 2].copy_from_slice(&shndx.to_le_bytes());
            return Some(());
        }
        None
    }
}
