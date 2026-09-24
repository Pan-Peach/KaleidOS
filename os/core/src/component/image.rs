//! 组件镜像表：一份**常驻加载的组件代码**的 Core 真相。
//!
//! 身份模型（`docs/architecture/component-lifecycle.md` §2）：
//!
//! ```text
//! ComponentImageId  → 一次加载的代码：name / base / create / destroy / text_size / MemoryLease
//! ComponentId       → 一个跑起来的实例：state / image / instance_state（见 registry.rs）
//! ```
//!
//! 规则：
//! - **一个 artifact 名只有一份 image**：同名再次加载 = 复用 image、产生**新实例**
//!   （新 `ComponentId`、新 state），不再拒绝。
//! - **image 记录部署域（`ExecutionDomain`）**：放段结果只在对应执行域里有意义
//!   （KernelNative = 共享内核 AS 的 VA / 裸 Core 地址；Isolated = 实例私有 AS
//!   的 VA）。**跨域复用 = 静默降级**，必须由装载门禁显式拒绝
//!   （`component/load.rs`）。
//! - **Isolated 的按域段规划（`placement`）随 image 常驻**：同域的第二/第 N 个
//!   实例把**同一份 backing** 映射进自己的私有 AS（`isolated_lifecycle`）——
//!   text / rodata 共享，`.data` / `.bss` 仍是 **image-global**（与 KernelNative
//!   同一契约，见 `docs/architecture/component-lifecycle.md` §9）；per-instance
//!   状态由实例窗口（Core 预置 backing）承载。
//! - **image 永不 unload**（pinned-until-reboot）：不实现 `instances == 0 → free`，
//!   Stopped/Failed 实例记录留作 tombstone（契约 §8/§9）。
//! - **`MemoryLease` 归 image 所有**；authority（MMIO/IRQ/DMA）、任务、接口发布
//!   归实例（`ComponentId`）。

use crate::component::endpoint::ExecutionDomain;
use crate::component::loader::LoadedComponent;
use crate::memory::MemoryLease;
use crate::memory::address_space::Mapping;
use alloc::vec::Vec;
use spin::{Mutex, Once};

/// artifact 名上限（与旧 registry 一致；超出即拒绝，不截断）。
const MAX_NAME_LEN: usize = 64;

/// 组件镜像身份（Identity，不是 Authority）。Core 分配、单调递增、不回收。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ComponentImageId(u32);

impl ComponentImageId {
    /// 身份可从 raw 值构造（Core 记录 / trace / 未来 IPC 用）；存在性由表校验。
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// 原始编号（供 Core 记录与 trace 使用）。
    pub const fn raw(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageError {
    /// artifact 名超过 [`MAX_NAME_LEN`]。
    NameTooLong,
    /// image id 空间耗尽（单调递增）。
    IdExhausted,
}

/// 一份常驻加载的组件代码。
#[derive(Debug, PartialEq)]
pub struct ComponentImage {
    pub id: ComponentImageId,
    /// artifact 名（不含 `.kcomp` 后缀；唯一性锚在这里，不在实例上）。
    pub name: Vec<u8>,
    /// 段放置基址（`[base, base + text_size)` 是装载镜像区间）。
    pub base: usize,
    /// `kcomp_instance_create` 入口地址（放在 Core-owned 栈上调用）。
    pub create: usize,
    /// `kcomp_instance_destroy` 入口地址（停止路径在 Core-owned 栈上调用）。
    pub destroy: usize,
    /// **可选**的 `kcomp_service_dispatch` 入口地址（loader 解析 + 已分配
    /// executable 段边界校验；**provider 域内**的 VA：KernelNative = Core AS，
    /// Isolated = 该实例私有 AS）。`None` = 组件不提供 endpoint 服务。
    pub service_dispatch: Option<usize>,
    /// 装载镜像大小（loader 的放段结果；曾在此处被丢弃，拆分后保留）。
    pub text_size: usize,
    /// 组件 `kcomp_abi` 的已校验值（loader 放段后读取，见 loader.rs）。
    pub abi: u64,
    /// **部署真相**：这份放段结果属于哪个执行域。放段 / 重定位的 VA 只在对应
    /// 域里有意义，跨域复用必须显式拒绝（`component/load.rs` 的门禁）。
    pub domain: ExecutionDomain,
    /// **Isolated 按域放段的段规划**（VA → backing PA，逐段权限）；KernelNative
    /// 为空。常驻随 image（lease 归 image）：同域的第二/第 N 个实例把这份
    /// backing 映射进自己的私有 AS（`isolated_lifecycle`）。
    pub(crate) placement: Vec<Mapping>,
    /// 常驻段 lease：**归 image 所有**。phase 1 不回收（physical residency）。
    pub(crate) memory: MemoryLease,
}

/// 组件镜像表（Core 保留的 image 真相）。可构造（测试友好），生产用全局 `init`。
pub struct ImageTable {
    images: Vec<ComponentImage>,
    next_id: u64,
}

impl ImageTable {
    pub fn new() -> Self {
        Self {
            images: Vec::new(),
            next_id: 1,
        }
    }

    /// 登记一份加载完成的 image（含部署域与按域段规划）。
    ///
    /// 同名已存在时**不重复登记**，返回已有 id（调用方应先 `find`，避免白做一次
    /// 加载；即使竞争也不会产生第二份同名 image——新 lease 随 `loaded` 释放）。
    /// **不校验部署域**：跨域复用由装载门禁在登记之前显式拒绝（这里是 Core 内部
    /// 记账，不发明第二套策略）。
    pub fn register(
        &mut self,
        name: &[u8],
        mut loaded: LoadedComponent,
        domain: ExecutionDomain,
        placement: Vec<Mapping>,
    ) -> Result<ComponentImageId, ImageError> {
        if name.len() > MAX_NAME_LEN {
            return Err(ImageError::NameTooLong);
        }
        if let Some(id) = self.find(name) {
            return Ok(id);
        }
        let id = ComponentImageId::from_raw(
            u32::try_from(self.next_id).map_err(|_| ImageError::IdExhausted)?,
        );
        self.next_id += 1;
        self.images.push(ComponentImage {
            id,
            name: name.to_vec(),
            base: loaded.base,
            create: loaded.create,
            destroy: loaded.destroy,
            service_dispatch: loaded.service_dispatch,
            text_size: loaded.text_size,
            abi: loaded.abi,
            domain,
            placement,
            // loader 成功返回必然携带 lease（放段 = 一次 region 分配）。
            memory: loaded.take_memory().expect("loader always returns a lease"),
        });
        Ok(id)
    }

    /// 按 artifact 名精确查找（唯一性锚点）。
    pub fn find(&self, name: &[u8]) -> Option<ComponentImageId> {
        self.images
            .iter()
            .find(|image| image.name.as_slice() == name)
            .map(|image| image.id)
    }

    pub fn get(&self, id: ComponentImageId) -> Option<&ComponentImage> {
        self.images.iter().find(|image| image.id == id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &ComponentImage> {
        self.images.iter()
    }

    pub fn len(&self) -> usize {
        self.images.len()
    }

    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }
}

impl Default for ImageTable {
    fn default() -> Self {
        Self::new()
    }
}

// —— 全局（boot/core::init 初始化；monitor 等使用全局，测试用 ImageTable::new()）——

static IMAGES: Once<Mutex<ImageTable>> = Once::new();

/// 初始化全局镜像表（core::init 调用一次）。
pub fn init() {
    IMAGES.call_once(|| Mutex::new(ImageTable::new()));
}

/// 取全局镜像表（init 后可用）。
pub fn get_images() -> &'static Mutex<ImageTable> {
    IMAGES.get().expect("image table not initialized")
}

/// 测试专用：直接登记一份假 image（不跑 loader），给 registry/handle/task 的
/// host 用例提供真实的 image 身份与常驻 lease。调用方须持有 memory GUARD。
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::component::containment::KCOMP_ABI;
    use crate::component::loader::LoadedComponent;

    pub(crate) fn register_test_image(name: &[u8], destroy: usize) -> ComponentImageId {
        register_test_image_with_dispatch(name, destroy, None)
    }

    /// 带**可选** `kcomp_service_dispatch` 的测试 image：endpoint call 用例用它
    /// 区分"有 dispatcher"与"没有 dispatcher"两条路径。
    pub(crate) fn register_test_image_with_dispatch(
        name: &[u8],
        destroy: usize,
        service_dispatch: Option<usize>,
    ) -> ComponentImageId {
        register_test_image_in_domain(
            name,
            destroy,
            service_dispatch,
            ExecutionDomain::KernelNative,
        )
    }

    /// 显式指定部署域的测试 image（Isolated 复用 / 跨域拒绝用例）。
    pub(crate) fn register_test_image_in_domain(
        name: &[u8],
        destroy: usize,
        service_dispatch: Option<usize>,
        domain: ExecutionDomain,
    ) -> ComponentImageId {
        let lease = crate::memory::alloc_region(crate::memory::ALLOC_GRANULE).unwrap();
        let base = lease.region().base;
        get_images()
            .lock()
            .register(
                name,
                LoadedComponent {
                    base,
                    create: base + 8,
                    destroy,
                    service_dispatch,
                    text_size: 64,
                    abi: KCOMP_ABI,
                    memory: Some(lease),
                },
                domain,
                Vec::new(),
            )
            .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::containment::KCOMP_ABI;

    /// 一份可登记的假 image（只测表语义，不跑 loader）。
    fn loaded() -> LoadedComponent {
        let lease = crate::memory::alloc_region(crate::memory::ALLOC_GRANULE).unwrap();
        let base = lease.region().base;
        LoadedComponent {
            base,
            create: base + 8,
            destroy: base + 16,
            service_dispatch: None,
            text_size: 64,
            abi: KCOMP_ABI,
            memory: Some(lease),
        }
    }

    /// 一条 Isolated 段规划（表语义用例：只记真相，不落页表）。
    fn placement_entry(base: usize) -> Mapping {
        Mapping {
            virtual_range: crate::memory::address_space::VirtualRange { base, size: 4096 },
            physical_range: crate::memory::address_space::PhysicalRange {
                base: base + 0x1000,
                size: 4096,
            },
            permission: crate::memory::address_space::MappingPermission::READ
                | crate::memory::address_space::MappingPermission::EXECUTE,
        }
    }

    #[test]
    fn register_assigns_monotonic_ids_and_find_resolves_name() {
        let _guard = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();

        let mut table = ImageTable::new();
        let a = table
            .register(b"a", loaded(), ExecutionDomain::KernelNative, Vec::new())
            .unwrap();
        let b = table
            .register(b"b", loaded(), ExecutionDomain::KernelNative, Vec::new())
            .unwrap();
        assert_eq!(a.raw(), 1);
        assert_eq!(b.raw(), 2);
        assert_eq!(table.find(b"a"), Some(a));
        assert_eq!(table.find(b"b"), Some(b));
        assert_eq!(table.find(b"missing"), None);
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn same_name_reuses_one_image() {
        let _guard = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();

        // Given：一份已登记的 image。
        let mut table = ImageTable::new();
        let first = table
            .register(b"dup", loaded(), ExecutionDomain::KernelNative, Vec::new())
            .unwrap();

        // When：同名再次登记（第二次加载的 lease 随参数释放）。
        let second = table
            .register(b"dup", loaded(), ExecutionDomain::KernelNative, Vec::new())
            .unwrap();

        // Then：复用同一 image，表不增长。
        assert_eq!(first, second);
        assert_eq!(table.len(), 1);
        assert_eq!(table.get(first).unwrap().name, b"dup");
    }

    #[test]
    fn record_owns_loader_placement_truth() {
        let _guard = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();

        let mut table = ImageTable::new();
        let id = table
            .register(
                b"fields",
                loaded(),
                ExecutionDomain::KernelNative,
                Vec::new(),
            )
            .unwrap();
        let image = table.get(id).unwrap();
        assert_eq!(image.base + 8, image.create);
        assert_eq!(image.base + 16, image.destroy);
        assert_eq!(image.service_dispatch, None, "可选入口缺省不携带");
        assert_eq!(image.text_size, 64);
        assert_eq!(image.abi, KCOMP_ABI);
        assert_eq!(image.domain, ExecutionDomain::KernelNative);
        assert!(image.placement.is_empty(), "KernelNative 没有按域段规划");
        assert!(image.memory.size() >= crate::memory::ALLOC_GRANULE);
    }

    /// Isolated 按域段规划随 image 常驻（重启复用的真相：第二个实例按同一份
    /// VA→PA 计划映射自己的私有 AS）。
    #[test]
    fn isolated_placement_is_recorded_with_the_image() {
        let _guard = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();

        let lease = crate::memory::alloc_region(crate::memory::ALLOC_GRANULE).unwrap();
        let base = lease.region().base;
        let placement = alloc::vec![placement_entry(0x2000_0000)];
        let mut table = ImageTable::new();
        let id = table
            .register(
                b"isolated_plan",
                LoadedComponent {
                    base,
                    create: base + 8,
                    destroy: base + 16,
                    service_dispatch: None,
                    text_size: 64,
                    abi: KCOMP_ABI,
                    memory: Some(lease),
                },
                ExecutionDomain::IsolatedNative,
                placement.clone(),
            )
            .unwrap();
        let image = table.get(id).unwrap();
        assert_eq!(image.domain, ExecutionDomain::IsolatedNative);
        assert_eq!(image.placement, placement);
    }

    /// `service_dispatch` 是可选入口：loader 解析到就原样带进 image 真相
    /// （`None` / `Some` 都是合法形态）。
    #[test]
    fn optional_service_dispatch_is_carried_through_registration() {
        let _guard = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();

        let mut table = ImageTable::new();
        let mut loaded = loaded();
        loaded.service_dispatch = Some(loaded.base + 32);
        let expected = loaded.service_dispatch;
        let id = table
            .register(
                b"dispatch",
                loaded,
                ExecutionDomain::KernelNative,
                Vec::new(),
            )
            .unwrap();
        assert_eq!(table.get(id).unwrap().service_dispatch, expected);
    }

    #[test]
    fn name_too_long_is_rejected() {
        let mut table = ImageTable::new();
        let long = [b'x'; MAX_NAME_LEN + 1];
        assert_eq!(
            table.register(&long, loaded(), ExecutionDomain::KernelNative, Vec::new()),
            Err(ImageError::NameTooLong)
        );
        assert!(table.is_empty());
    }

    #[test]
    fn unknown_image_id_is_none() {
        let table = ImageTable::new();
        assert!(table.get(ComponentImageId::from_raw(99)).is_none());
    }
}
