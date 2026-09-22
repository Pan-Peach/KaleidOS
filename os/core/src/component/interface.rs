//! Component Interface Registry —— 组件→组件依赖的唯一机制（**骨架**）。
//!
//! # 规则（本轮定案）
//!
//! ```text
//! Component → Core       = Core Export ABI（component/export.rs，ELF undefined symbol）
//! Component → Component  = Interface binding（本模块）——禁止 flat ELF symbol 互链
//! ```
//!
//! 组件替换的成立条件：consumer 拿的是**逻辑 binding**（`BindingId` + 当前
//! `api/ctx/generation`），不是"永不变更的 provider ELF 符号地址"。provider
//! 换成新实现时，consumer 只需 `refresh` 拿新 `api/ctx`，**不需要 ELF reload**。
//!
//! # 数据模型
//!
//! - `InterfaceId`：Core 分配的接口身份（每个唯一接口名一条记录）。
//! - `BindingId`：一次 publish 产生的绑定槽；unbind/rebind **不失效**——
//!   unbind 只清 provider，槽位保留（consumer 持有的 id 继续有效）。
//! - `InterfaceAbi`：**exact ABI fingerprint**（`#[repr(transparent)] u64`）。
//!   它没有"版本兼容"语义，只回答一个问题：provider 与 consumer 是否由完全
//!   相同的 Service ABI contract 编译？不一致必须拒绝 binding/replacement。
//!   **绝不允许把布局不同的 function table 交给 consumer。**
//! - provider state：`provider: Option<ComponentId>`（None ⇔ Unbound）。
//! - `api: *const ()`：provider 提供的 `#[repr(C)]` function table（opaque）。
//! - `ctx: *mut ()`：provider opaque state/context（opaque）。
//! - `generation: u64`：每次成功 commit 新 provider 时前进一次。
//!
//! **Core 永远不解引用 `api` / `ctx`**：function table 的内容由 provider 与
//! consumer 共享的 SDK contract 决定，Core 只存取指针。
//!
//! # 阶段一调用方式（KernelNative，direct call）
//!
//! ```rust
//! #[repr(C)]
//! struct SampleService {
//!     do_thing: extern "C" fn(ctx: *mut (), u32) -> u32,
//! }
//! // consumer: let view = interfaces.bind(&reg, b"sample", Kind::Service, ABI)?;
//! //           let vtable = unsafe { &*(view.api.cast::<SampleService>()) };
//! //           (vtable.do_thing)(view.ctx, 5);
//! ```
//!
//! # Staged publish（`Declared → Resolved → Starting → Ready`）
//!
//! `kcomp_instance_create()` 执行期间调用 publish **不会立即修改 active binding**：它记录为
//! 该组件的 pending publication。Core 在 `kcomp_instance_create()` 返回 0 后**原子提交**
//! 该组件的 pending interfaces（见 [`InterfaceRegistry::commit_pending`]）：
//!
//! - interface 不存在 → 新建 binding（generation = 1）；
//! - 已存在且 ABI fingerprint 相同 → 保留原 `BindingId`，替换 provider/api/ctx，
//!   `generation += 1`（hot replacement 的全部范围）；
//! - 已存在但 ABI fingerprint 不同 → **拒绝 replacement**（当前阶段）。
//!
//! init 失败或 panic：丢弃该组件全部 pending，旧 active provider **完全不受影响**
//! （见 [`InterfaceRegistry::discard_pending`] / `failure::fail_component`）。
//!
//! # 下一阶段（seam / TODO）
//!
//! - ABI fingerprint 的**具体定义**（类型布局 → 稳定 u64）未来在 `kcomp-sdk`
//!   统一生成；Core 不做 ABI hash / proc macro，只保留 u64 机制。
//! - compatible range（major/minor 版本区间）替代 exact match：本轮不做。
//! - 组件失败/卸载时 Core 自动 unbind（本轮提供原语并已接线 fail_component）。
//! - dependency graph / requires manifest / ABI-changing coordinated update /
//!   automatic consumer reload / multi-version ABI：全部 deferred（仅此文件保留 seam）。
//! - 传输升级（IPC / Wasm host call）不改 binding 数据模型。

use alloc::vec::Vec;

use crate::component::registry::Registry;
use crate::component::{ComponentId, ComponentState};
use spin::{Mutex, Once};

pub use crate::generated::abi::InterfaceKind;

/// Exact ABI fingerprint（`#[repr(transparent)]`，无版本兼容语义）。
///
/// 只回答："provider 与 consumer 是否由**完全相同**的 Service ABI contract
/// 编译？" 不一致 → `InterfaceError::AbiMismatch` → 拒绝 binding/replacement。
///
/// TODO(service-abi): 具体 Service contract 的 fingerprint 未来在 `kcomp-sdk`
/// 统一定义（例如由 contract 布局经稳定哈希生成）；当前阶段 Core 只提供 u64
/// 机制与 seam，不实现 ABI hash 生成器或 proc macro。
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InterfaceAbi(u64);

impl InterfaceAbi {
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Core 分配的接口身份（Identity，不是 Authority）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InterfaceId(u32);

impl InterfaceId {
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// 绑定槽身份：consumer 持有的逻辑 handle。unbind/rebind 不失效。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BindingId(u32);

impl BindingId {
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// 一次 `bind` / `refresh` 返回的逻辑 binding 视图（Copy，无借用）。
/// consumer 把 `api` cast 成自己声明的 `#[repr(C)]` function table，并把
/// `ctx` 作为 opaque state 传入。
#[derive(Debug, Clone, Copy)]
pub struct BindingView {
    pub id: BindingId,
    pub interface: InterfaceId,
    pub abi: InterfaceAbi,
    pub provider: ComponentId,
    /// provider 的 `#[repr(C)]` function table 指针；消费方按接口契约 cast。
    /// 生命周期：provider Ready 期间有效；unbind/组件失败后必须视为悬垂——
    /// consumer 应通过 `refresh` 重新获取。
    pub api: *const (),
    /// provider opaque state/context；由 consumer 原样传给 function table。
    pub ctx: *mut (),
    /// provider commit 计数（每次成功替换 +1）。
    pub generation: u64,
}

impl PartialEq for BindingView {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.interface == other.interface
            && self.abi == other.abi
            && self.provider == other.provider
            && core::ptr::eq(self.api, other.api)
            && core::ptr::eq(self.ctx, other.ctx)
            && self.generation == other.generation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterfaceError {
    /// publish：provider 不在组件注册表。
    ProviderNotFound,
    /// publish：provider 不在可初始化状态（`Starting` / `Ready` 之外）。
    ProviderNotReady,
    /// bind：接口名未知。
    UnknownInterface,
    /// bind / commit：同接口名已用不同 kind 发布（一个名字一个 kind）。
    KindMismatch,
    /// bind / refresh / commit：ABI fingerprint 不一致（exact match 失败）。
    AbiMismatch,
    /// bind：绑定槽存在但 provider 已 unbind。
    Unbound,
    /// refresh：无效槽 id。
    BindingNotFound,
    /// InterfaceId / BindingId 空间耗尽（u32 单调递增）。
    IdExhausted,
}

/// 一条接口记录（按名字唯一）。
struct InterfaceRecord {
    id: InterfaceId,
    name: Vec<u8>,
    kind: InterfaceKind,
}

/// 一个绑定槽：interface + 当前 provider / ABI / function table / context / generation。
/// `provider == None` ⇔ Unbound（槽保留，供重绑，binding id 不失效）。
struct BindingRecord {
    interface_id: InterfaceId,
    abi: InterfaceAbi,
    provider: Option<ComponentId>,
    api: *const (),
    ctx: *mut (),
    generation: u64,
}

/// `kcomp_instance_create` 期间记录的一次待提交发布（staged publish）。
struct PendingPublication {
    component: ComponentId,
    name: Vec<u8>,
    kind: InterfaceKind,
    abi: InterfaceAbi,
    api: *const (),
    ctx: *mut (),
}

// `api` / `ctx` 是 opaque provider 指针：Registry 只存取、永不解引用。
// Send/Sync 安全（与 export.rs 的 ExportAddress 同一理由；跨线程使用由
// 外层 Mutex 串行化）。
unsafe impl Send for BindingRecord {}
unsafe impl Sync for BindingRecord {}
unsafe impl Send for PendingPublication {}
unsafe impl Sync for PendingPublication {}

/// 组件→组件 依赖的 Core 真相：谁提供了什么接口、当前绑到谁。
pub struct InterfaceRegistry {
    interfaces: Vec<InterfaceRecord>,
    bindings: Vec<BindingRecord>,
    pending: Vec<PendingPublication>,
    next_interface_id: u32,
}

impl InterfaceRegistry {
    pub fn new() -> Self {
        Self {
            interfaces: Vec::new(),
            bindings: Vec::new(),
            pending: Vec::new(),
            next_interface_id: 1,
        }
    }

    /// **Staged publish**：记录一条 pending publication，不修改 active binding。
    ///
    /// 由 `kcore_interface_publish` 在 `kcomp_instance_create()` 执行期间调用（provider 此时
    /// 处于 `Starting`）。Core 校验 provider 存在且处于可初始化状态，但**不**
    /// 在此刻提交——提交由 [`Self::commit_pending`] 在 init 成功后完成。
    ///
    /// 返回 `Ok(())` 只表示"已记录 pending"；真正的接口身份/ABI 冲突在 commit
    /// 阶段统一判定（原子语义）。
    #[allow(clippy::too_many_arguments)]
    pub fn stage_publish(
        &mut self,
        components: &Registry,
        provider: ComponentId,
        name: &[u8],
        kind: InterfaceKind,
        abi: InterfaceAbi,
        api: *const (),
        ctx: *mut (),
    ) -> Result<(), InterfaceError> {
        let rec = components
            .get(provider)
            .ok_or(InterfaceError::ProviderNotFound)?;
        // `Starting` = 正在 call_init；`Ready` 允许未来 monitor 驱动的重发布。
        if !matches!(rec.state, ComponentState::Starting | ComponentState::Ready) {
            return Err(InterfaceError::ProviderNotReady);
        }
        self.pending.push(PendingPublication {
            component: provider,
            name: name.to_vec(),
            kind,
            abi,
            api,
            ctx,
        });
        Ok(())
    }

    /// 提交某组件的全部 pending publications（`kcomp_instance_create()` 返回 0 后由 Core 调用）。
    ///
    /// **原子语义**：先整体校验该组件的 pending（kind / ABI 冲突），任一失败则
    /// 丢弃该组件全部 pending 并返回 `Err`——旧 active binding 完全不受影响；
    /// 校验通过后一次性应用（hot replacement 规则见模块文档）。
    pub fn commit_pending(
        &mut self,
        components: &Registry,
        component: ComponentId,
    ) -> Result<(), InterfaceError> {
        // 仅取出该组件的 pending（其它组件的 pending 原样保留）。
        let mut staging = Vec::new();
        self.pending.retain(|p| {
            if p.component == component {
                staging.push(PendingPublication {
                    component: p.component,
                    name: p.name.clone(),
                    kind: p.kind,
                    abi: p.abi,
                    api: p.api,
                    ctx: p.ctx,
                });
                false
            } else {
                true
            }
        });

        if components.get(component).is_none() {
            return Err(InterfaceError::ProviderNotFound);
        }

        // 校验阶段：任一冲突 → 全部丢弃（staging 随作用域结束被 drop）。
        for p in &staging {
            if let Some(iface) = self.interfaces.iter().find(|r| r.name == p.name) {
                if iface.kind != p.kind {
                    return Err(InterfaceError::KindMismatch);
                }
                if let Some(binding) = self.bindings.iter().find(|b| b.interface_id == iface.id)
                    && binding.abi != p.abi
                {
                    return Err(InterfaceError::AbiMismatch);
                }
            }
        }

        // 应用阶段：接口已存在且 ABI 相同 → 保留 BindingId、generation += 1。
        for p in staging {
            self.apply_publish(p)?;
        }
        Ok(())
    }

    /// 丢弃某组件的全部 pending publications（init 失败 / panic 路径）。
    /// 旧 active binding 完全不受影响。
    pub fn discard_pending(&mut self, component: ComponentId) {
        self.pending.retain(|p| p.component != component);
    }

    /// 应用一条已校验的 pending publication。
    fn apply_publish(&mut self, p: PendingPublication) -> Result<(), InterfaceError> {
        let interface_id = match self.interfaces.iter().find(|r| r.name == p.name) {
            Some(record) => record.id,
            None => {
                let id = InterfaceId::from_raw(self.next_interface_id);
                self.next_interface_id = self
                    .next_interface_id
                    .checked_add(1)
                    .ok_or(InterfaceError::IdExhausted)?;
                self.interfaces.push(InterfaceRecord {
                    id,
                    name: p.name,
                    kind: p.kind,
                });
                id
            }
        };

        match self
            .bindings
            .iter()
            .position(|b| b.interface_id == interface_id)
        {
            Some(i) => {
                // 同 ABI replacement：保留 BindingId（槽位），推进 generation。
                let binding = &mut self.bindings[i];
                binding.abi = p.abi;
                binding.provider = Some(p.component);
                binding.api = p.api;
                binding.ctx = p.ctx;
                binding.generation = binding.generation.wrapping_add(1);
            }
            None => {
                self.bindings.push(BindingRecord {
                    interface_id,
                    abi: p.abi,
                    provider: Some(p.component),
                    api: p.api,
                    ctx: p.ctx,
                    generation: 1,
                });
            }
        }
        Ok(())
    }

    /// consumer 按名 bind：查找接口 → exact-compare ABI fingerprint → 验证当前
    /// provider 存活（存在且 Ready）→ 返回稳定 `BindingId` + 当前 `api/ctx/generation`。
    pub fn bind(
        &self,
        components: &Registry,
        name: &[u8],
        kind: InterfaceKind,
        abi: InterfaceAbi,
    ) -> Result<BindingView, InterfaceError> {
        let interface = self
            .interfaces
            .iter()
            .find(|r| r.name == name)
            .ok_or(InterfaceError::UnknownInterface)?;
        if interface.kind != kind {
            return Err(InterfaceError::KindMismatch);
        }
        let index = self
            .bindings
            .iter()
            .position(|b| b.interface_id == interface.id)
            .ok_or(InterfaceError::Unbound)?;
        if self.bindings[index].abi != abi {
            return Err(InterfaceError::AbiMismatch);
        }
        let view = self.view_of(components, index)?;
        // Trace：一次成功的 bind 解析（Core 不记录 consumer 边，见 TraceEvent 文档）。
        crate::trace::emit(crate::trace::TraceEvent::InterfaceBind {
            consumer: None,
            provider: view.provider,
            interface: view.interface,
        });
        Ok(view)
    }

    /// consumer 用已有 `BindingId` refresh：exact-compare 期望 ABI → 重新验证
    /// provider → 返回最新 `api/ctx/generation`（provider 替换后无需 ELF reload）。
    pub fn refresh(
        &self,
        components: &Registry,
        id: BindingId,
        abi: InterfaceAbi,
    ) -> Result<BindingView, InterfaceError> {
        let index = id.raw() as usize;
        let binding = self
            .bindings
            .get(index)
            .ok_or(InterfaceError::BindingNotFound)?;
        if binding.abi != abi {
            return Err(InterfaceError::AbiMismatch);
        }
        let view = self.view_of(components, index)?;
        // Trace：刷新会暴露 provider 是否被替换（generation）。首次 bind 是 1，
        // 同 ABI 热替换后每次 +1。
        crate::trace::emit(crate::trace::TraceEvent::InterfaceRefresh {
            binding: view.id,
            generation: view.generation,
        });
        Ok(view)
    }

    /// 生成视图，并做 provider 存活二次校验（Core 验证后才交付）。
    fn view_of(&self, components: &Registry, index: usize) -> Result<BindingView, InterfaceError> {
        let binding = self
            .bindings
            .get(index)
            .ok_or(InterfaceError::BindingNotFound)?;
        let provider = binding.provider.ok_or(InterfaceError::Unbound)?;
        // 存活二次校验：provider 必须仍在组件注册表且处于 Ready。
        // （组件卸载/失败后，binding 立即不可用，不留给 consumer 悬垂调用。）
        let rec = components
            .get(provider)
            .ok_or(InterfaceError::ProviderNotFound)?;
        if rec.state != ComponentState::Ready {
            return Err(InterfaceError::ProviderNotFound);
        }
        let interface = self
            .interfaces
            .iter()
            .find(|r| r.id == binding.interface_id)
            .expect("binding references a live interface record");
        Ok(BindingView {
            id: BindingId::from_raw(u32::try_from(index).map_err(|_| InterfaceError::IdExhausted)?),
            interface: interface.id,
            abi: binding.abi,
            provider,
            api: binding.api,
            ctx: binding.ctx,
            generation: binding.generation,
        })
    }

    /// 解除一个绑定槽的 provider（provider 停止提供该接口）。
    /// 槽保留：之后可重绑，consumer 的 id 继续有效。
    pub fn unbind(&mut self, id: BindingId) -> Result<(), InterfaceError> {
        let binding = self
            .bindings
            .get_mut(id.raw() as usize)
            .ok_or(InterfaceError::BindingNotFound)?;
        binding.provider = None;
        binding.api = core::ptr::null();
        binding.ctx = core::ptr::null_mut();
        Ok(())
    }

    /// 解除某 provider 的全部绑定（组件失败/卸载时由 Core 调用）。
    pub fn unbind_provider(&mut self, provider: ComponentId) {
        for binding in &mut self.bindings {
            if binding.provider == Some(provider) {
                binding.provider = None;
                binding.api = core::ptr::null();
                binding.ctx = core::ptr::null_mut();
            }
        }
    }

    pub fn interface_count(&self) -> usize {
        self.interfaces.len()
    }

    pub fn binding_count(&self) -> usize {
        self.bindings.len()
    }
}

impl Default for InterfaceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// —— 全局（boot/core::init 初始化；monitor 等使用全局，测试用 new()）——

static INTERFACES: Once<Mutex<InterfaceRegistry>> = Once::new();

/// 初始化全局接口注册表（core::init 调用一次）。
pub fn init() {
    INTERFACES.call_once(|| Mutex::new(InterfaceRegistry::new()));
}

/// 取全局接口注册表（init 后可用）。
pub fn get_interfaces() -> &'static Mutex<InterfaceRegistry> {
    INTERFACES
        .get()
        .expect("interface registry not initialized")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::image::ComponentImageId;
    use crate::component::registry::Registry;

    /// 测试用镜像身份：registry 只把它当身份键（image 表是另一份真相）。
    const IMAGE: ComponentImageId = ComponentImageId::from_raw(1);

    const ABI_A: InterfaceAbi = InterfaceAbi::from_raw(0xAAAA_0001);
    const ABI_B: InterfaceAbi = InterfaceAbi::from_raw(0xBBBB_0002);

    /// 测试用 dummy `#[repr(C)]` function table（不实现任何真实 Service）。
    /// `api` 指向这块 table（不是函数本身）；`ctx` 单独由 Core 交付。
    #[repr(C)]
    struct SampleService {
        do_thing: extern "C" fn(ctx: *mut (), input: u32) -> u32,
    }

    unsafe impl Sync for SampleService {}

    static SAMPLE_TABLE: SampleService = SampleService {
        do_thing: sample_impl,
    };

    extern "C" fn sample_impl(ctx: *mut (), input: u32) -> u32 {
        // provider 侧实现：ctx 指向一个计数器
        let counter = unsafe { &mut *(ctx as *mut u32) };
        *counter = counter.wrapping_add(input);
        *counter
    }

    /// 真实局部地址（区分 provider 即可，生命周期限测试函数内）。
    fn ctx(n: u32) -> *mut () {
        let mut slot = n;
        &mut slot as *mut u32 as *mut ()
    }

    fn api() -> *const () {
        &SAMPLE_TABLE as *const SampleService as *const ()
    }

    /// 发布一个接口并提交（provider 已 Ready），返回当前 BindingId。
    fn publish_ready(
        reg: &Registry,
        ifs: &mut InterfaceRegistry,
        provider: ComponentId,
        name: &[u8],
        kind: InterfaceKind,
        abi: InterfaceAbi,
        ctx: *mut (),
    ) -> BindingId {
        ifs.stage_publish(reg, provider, name, kind, abi, api(), ctx)
            .unwrap();
        ifs.commit_pending(reg, provider).unwrap();
        ifs.bind(reg, name, kind, abi).unwrap().id
    }

    /// 构造注册表并声明三个 Ready 实例（provider_a / provider_b / consumer）。
    fn ready_world() -> (Registry, Vec<ComponentId>) {
        let mut reg = Registry::new();
        let mut ids = Vec::new();
        for _ in 0..3 {
            let id = reg.declare(IMAGE).unwrap();
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            reg.finish_start(id).unwrap();
            ids.push(id);
        }
        (reg, ids)
    }

    /// 性能基线（`make bench`）：**interface 调用开销** —— 直接 Rust 调用 vs
    /// 经 `#[repr(C)]` function table 调用 vs 经 Registry 解析后的表调用。
    ///
    /// 刻意分开报：hot path（`direct_call` / `table_call`，同一个函数，区别只在
    /// 是否过表）与 control path（`bind` / `refresh` / `publish`，一次性），
    /// 混成一个数字就没有意义了。
    #[test]
    #[ignore = "性能基线：make bench 手动跑"]
    fn bench_interface_call_paths() {
        static mut COUNTER: u32 = 0;
        let stable_ctx = core::ptr::addr_of_mut!(COUNTER) as *mut ();

        let (reg, ids) = ready_world();
        let mut ifs = InterfaceRegistry::new();
        let provider = ids[0];
        let binding = publish_ready(
            &reg,
            &mut ifs,
            provider,
            b"sample",
            InterfaceKind::Service,
            ABI_A,
            stable_ctx,
        );
        let view = ifs
            .bind(&reg, b"sample", InterfaceKind::Service, ABI_A)
            .unwrap();
        // SAFETY: `api` 由 Core 在 bind 时交付，指向 SAMPLE_TABLE（本测试内有效）。
        let table = unsafe { &*(view.api as *const SampleService) };

        crate::bench::report_environment();

        crate::bench::run("interface.direct_call", 10_000, || {
            sample_impl(stable_ctx, 1)
        })
        .report();

        crate::bench::run("interface.table_call", 10_000, || {
            (table.do_thing)(stable_ctx, 1)
        })
        .report();

        let mut bind = crate::bench::Bench::new("interface.bind");
        bind.run(1_000, || {
            ifs.bind(&reg, b"sample", InterfaceKind::Service, ABI_A)
                .unwrap()
        });
        bind.finish().report();

        let mut refresh = crate::bench::Bench::new("interface.refresh");
        refresh.run(1_000, || ifs.refresh(&reg, binding, ABI_A).unwrap());
        refresh.finish().report();

        let mut publish = crate::bench::Bench::new("interface.publish");
        publish.run(100, || {
            ifs.stage_publish(
                &reg,
                provider,
                b"sample",
                InterfaceKind::Service,
                ABI_A,
                api(),
                stable_ctx,
            )
            .unwrap();
            ifs.commit_pending(&reg, provider).unwrap()
        });
        publish.finish().report();
    }

    /// 性能基线（`make bench`）：Interface Registry 的**规模趋势**。
    ///
    /// 查找是线性扫描（`Vec`），所以测 N = 1/8/32/128 个接口时 `bind` 第一个
    /// （最坏情况）的成本。**先证明它是不是真问题，再决定要不要加索引**。
    #[test]
    #[ignore = "性能基线：make bench 手动跑"]
    fn bench_registry_bind_scaling() {
        static mut COUNTER: u32 = 0;
        let stable_ctx = core::ptr::addr_of_mut!(COUNTER) as *mut ();

        crate::bench::report_environment();
        const SCALING: [(usize, &str); 4] = [
            (1, "registry.bind.n1"),
            (8, "registry.bind.n8"),
            (32, "registry.bind.n32"),
            (128, "registry.bind.n128"),
        ];
        for (count, name) in SCALING {
            let (reg, ids) = ready_world();
            let mut ifs = InterfaceRegistry::new();
            let provider = ids[0];
            for index in 0..count {
                let iface = alloc::format!("iface{index}").into_bytes();
                publish_ready(
                    &reg,
                    &mut ifs,
                    provider,
                    &iface,
                    InterfaceKind::Service,
                    ABI_A,
                    stable_ctx,
                );
            }
            let mut bench = crate::bench::Bench::new(name);
            bench.run(100, || {
                ifs.bind(&reg, b"iface0", InterfaceKind::Service, ABI_A)
                    .unwrap()
            });
            bench.finish().report();
        }
    }

    // -- 1. staged publish + commit ----------------------------------------

    #[test]
    fn staged_publish_does_not_touch_active_binding_until_commit() {
        let (reg, ids) = ready_world();
        let mut ifs = InterfaceRegistry::new();
        ifs.stage_publish(
            &reg,
            ids[0],
            b"sample",
            InterfaceKind::Service,
            ABI_A,
            api(),
            ctx(1),
        )
        .unwrap();
        // 尚未 commit：接口不可见（不会交出半成品 vtable）。
        assert_eq!(
            ifs.bind(&reg, b"sample", InterfaceKind::Service, ABI_A),
            Err(InterfaceError::UnknownInterface)
        );
        assert_eq!(ifs.binding_count(), 0);
        ifs.commit_pending(&reg, ids[0]).unwrap();
        let view = ifs
            .bind(&reg, b"sample", InterfaceKind::Service, ABI_A)
            .unwrap();
        assert_eq!(view.provider, ids[0]);
        assert_eq!(view.api, api());
        assert_eq!(view.ctx, ctx(1));
        assert_eq!(view.generation, 1, "首次 commit = generation 1");
    }

    #[test]
    fn stage_publish_rejects_unknown_provider() {
        let (reg, _ids) = ready_world();
        let mut ifs = InterfaceRegistry::new();
        assert_eq!(
            ifs.stage_publish(
                &reg,
                ComponentId::from_raw(99),
                b"sample",
                InterfaceKind::Service,
                ABI_A,
                core::ptr::null(),
                core::ptr::null_mut(),
            ),
            Err(InterfaceError::ProviderNotFound)
        );
    }

    #[test]
    fn stage_publish_rejects_non_starting_provider() {
        let mut reg = Registry::new();
        let declared = reg.declare(IMAGE).unwrap();
        // 不 resolve/begin_start：保持 Declared
        let mut ifs = InterfaceRegistry::new();
        assert_eq!(
            ifs.stage_publish(
                &reg,
                declared,
                b"sample",
                InterfaceKind::Service,
                ABI_A,
                core::ptr::null(),
                core::ptr::null_mut(),
            ),
            Err(InterfaceError::ProviderNotReady)
        );
    }

    #[test]
    fn same_name_different_kind_is_rejected_at_commit() {
        let (reg, ids) = ready_world();
        let mut ifs = InterfaceRegistry::new();
        publish_ready(
            &reg,
            &mut ifs,
            ids[0],
            b"sample",
            InterfaceKind::Service,
            ABI_A,
            ctx(1),
        );
        // provider_b 以不同 kind 发布同名接口 → commit 拒绝。
        ifs.stage_publish(
            &reg,
            ids[1],
            b"sample",
            InterfaceKind::Device,
            ABI_A,
            api(),
            ctx(2),
        )
        .unwrap();
        assert_eq!(
            ifs.commit_pending(&reg, ids[1]),
            Err(InterfaceError::KindMismatch)
        );
    }

    // -- 2. consumer bind + ABI mismatch -----------------------------------

    #[test]
    fn bind_unknown_interface_is_rejected() {
        let (reg, _ids) = ready_world();
        let ifs = InterfaceRegistry::new();
        assert_eq!(
            ifs.bind(&reg, b"nope", InterfaceKind::Service, ABI_A),
            Err(InterfaceError::UnknownInterface)
        );
    }

    #[test]
    fn bind_abi_mismatch_is_rejected() {
        let (reg, ids) = ready_world();
        let mut ifs = InterfaceRegistry::new();
        publish_ready(
            &reg,
            &mut ifs,
            ids[0],
            b"sample",
            InterfaceKind::Service,
            ABI_A,
            ctx(1),
        );
        assert_eq!(
            ifs.bind(&reg, b"sample", InterfaceKind::Service, ABI_B),
            Err(InterfaceError::AbiMismatch)
        );
    }

    #[test]
    fn bind_returns_current_api_ctx_and_generation() {
        let (reg, ids) = ready_world();
        let mut ifs = InterfaceRegistry::new();
        let binding = publish_ready(
            &reg,
            &mut ifs,
            ids[0],
            b"sample",
            InterfaceKind::Service,
            ABI_A,
            ctx(1),
        );
        let view = ifs
            .bind(&reg, b"sample", InterfaceKind::Service, ABI_A)
            .unwrap();
        assert_eq!(view.id, binding);
        assert_eq!(view.api, api());
        assert_eq!(view.ctx, ctx(1));
        assert_eq!(view.generation, 1);
    }

    // -- 3. unbind 后 binding 不可用 ---------------------------------------

    #[test]
    fn unbind_makes_binding_unavailable() {
        let (reg, ids) = ready_world();
        let mut ifs = InterfaceRegistry::new();
        let binding = publish_ready(
            &reg,
            &mut ifs,
            ids[0],
            b"sample",
            InterfaceKind::Service,
            ABI_A,
            ctx(1),
        );
        ifs.unbind(binding).unwrap();
        assert_eq!(
            ifs.bind(&reg, b"sample", InterfaceKind::Service, ABI_A),
            Err(InterfaceError::Unbound)
        );
        assert_eq!(
            ifs.refresh(&reg, binding, ABI_A),
            Err(InterfaceError::Unbound)
        );
    }

    #[test]
    fn unbind_unknown_binding_is_not_found() {
        let mut ifs = InterfaceRegistry::new();
        assert_eq!(
            ifs.unbind(BindingId::from_raw(7)),
            Err(InterfaceError::BindingNotFound)
        );
    }

    #[test]
    fn unbind_provider_revokes_all_its_bindings() {
        let (reg, ids) = ready_world();
        let mut ifs = InterfaceRegistry::new();
        publish_ready(
            &reg,
            &mut ifs,
            ids[0],
            b"svc_a",
            InterfaceKind::Service,
            ABI_A,
            ctx(1),
        );
        publish_ready(
            &reg,
            &mut ifs,
            ids[0],
            b"svc_b",
            InterfaceKind::Service,
            ABI_A,
            ctx(2),
        );
        ifs.unbind_provider(ids[0]);
        assert_eq!(ifs.binding_count(), 2, "槽保留");
        assert_eq!(
            ifs.bind(&reg, b"svc_a", InterfaceKind::Service, ABI_A),
            Err(InterfaceError::Unbound)
        );
        assert_eq!(
            ifs.bind(&reg, b"svc_b", InterfaceKind::Service, ABI_A),
            Err(InterfaceError::Unbound)
        );
        // 其他 provider 的绑定不受影响
        ifs.stage_publish(
            &reg,
            ids[1],
            b"svc_a",
            InterfaceKind::Service,
            ABI_A,
            api(),
            ctx(3),
        )
        .unwrap();
        ifs.commit_pending(&reg, ids[1]).unwrap();
        assert!(
            ifs.bind(&reg, b"svc_a", InterfaceKind::Service, ABI_A)
                .is_ok()
        );
    }

    // -- 4. hot replacement：同 ABI 保留 BindingId + generation 前进 ----------

    #[test]
    fn same_abi_replacement_keeps_binding_id_and_advances_generation() {
        let (reg, ids) = ready_world();
        let mut ifs = InterfaceRegistry::new();
        let binding = publish_ready(
            &reg,
            &mut ifs,
            ids[0],
            b"sample",
            InterfaceKind::Service,
            ABI_A,
            ctx(1),
        );
        let first = ifs
            .bind(&reg, b"sample", InterfaceKind::Service, ABI_A)
            .unwrap();
        assert_eq!(first.generation, 1);

        // provider_a 卸载 → Core 解绑；provider_b 以同 ABI 重发布。
        ifs.unbind_provider(ids[0]);
        ifs.stage_publish(
            &reg,
            ids[1],
            b"sample",
            InterfaceKind::Service,
            ABI_A,
            api(),
            ctx(2),
        )
        .unwrap();
        ifs.commit_pending(&reg, ids[1]).unwrap();

        // consumer 用旧 id refresh → 拿到新 provider 的 api/ctx，BindingId 不变。
        let view = ifs.refresh(&reg, binding, ABI_A).unwrap();
        assert_eq!(view.id, binding, "槽复用：consumer 持有的 id 不变");
        assert_eq!(view.provider, ids[1]);
        assert_eq!(view.ctx, ctx(2));
        assert_eq!(view.generation, 2, "replacement 后 generation 前进");
    }

    #[test]
    fn abi_mismatch_replacement_is_rejected_and_old_binding_survives() {
        let (reg, ids) = ready_world();
        let mut ifs = InterfaceRegistry::new();
        let binding = publish_ready(
            &reg,
            &mut ifs,
            ids[0],
            b"sample",
            InterfaceKind::Service,
            ABI_A,
            ctx(1),
        );

        // provider_b 用不同 ABI 尝试替换 → commit 拒绝，旧 binding 原样。
        ifs.stage_publish(
            &reg,
            ids[1],
            b"sample",
            InterfaceKind::Service,
            ABI_B,
            api(),
            ctx(2),
        )
        .unwrap();
        assert_eq!(
            ifs.commit_pending(&reg, ids[1]),
            Err(InterfaceError::AbiMismatch)
        );

        let view = ifs.refresh(&reg, binding, ABI_A).unwrap();
        assert_eq!(view.provider, ids[0], "旧 provider 不受影响");
        assert_eq!(view.ctx, ctx(1), "旧 ctx 不受影响");
        assert_eq!(view.generation, 1, "generation 不前进");
    }

    #[test]
    fn refresh_abi_mismatch_is_rejected() {
        let (reg, ids) = ready_world();
        let mut ifs = InterfaceRegistry::new();
        let binding = publish_ready(
            &reg,
            &mut ifs,
            ids[0],
            b"sample",
            InterfaceKind::Service,
            ABI_A,
            ctx(1),
        );
        assert_eq!(
            ifs.refresh(&reg, binding, ABI_B),
            Err(InterfaceError::AbiMismatch)
        );
    }

    // -- 5. provider failure / init failure 隔离 ----------------------------

    #[test]
    fn discard_pending_leaves_old_binding_uncontaminated() {
        let (reg, ids) = ready_world();
        let mut ifs = InterfaceRegistry::new();
        let binding = publish_ready(
            &reg,
            &mut ifs,
            ids[0],
            b"sample",
            InterfaceKind::Service,
            ABI_A,
            ctx(1),
        );

        // provider_b init 中途失败：pending 被丢弃，旧 active binding 完全不受影响。
        ifs.stage_publish(
            &reg,
            ids[1],
            b"sample",
            InterfaceKind::Service,
            ABI_A,
            api(),
            ctx(2),
        )
        .unwrap();
        ifs.discard_pending(ids[1]);

        let view = ifs.refresh(&reg, binding, ABI_A).unwrap();
        assert_eq!(view.provider, ids[0]);
        assert_eq!(view.ctx, ctx(1));
        assert_eq!(view.generation, 1);
    }

    #[test]
    fn commit_conflict_discards_all_pending_for_component() {
        let (reg, ids) = ready_world();
        let mut ifs = InterfaceRegistry::new();
        publish_ready(
            &reg,
            &mut ifs,
            ids[0],
            b"sample",
            InterfaceKind::Service,
            ABI_A,
            ctx(1),
        );

        // 同一组件两条 pending：一条合法新接口、一条 ABI 冲突 → 全部丢弃。
        ifs.stage_publish(
            &reg,
            ids[1],
            b"brand_new",
            InterfaceKind::Service,
            ABI_A,
            api(),
            ctx(2),
        )
        .unwrap();
        ifs.stage_publish(
            &reg,
            ids[1],
            b"sample",
            InterfaceKind::Service,
            ABI_B,
            api(),
            ctx(3),
        )
        .unwrap();
        assert_eq!(
            ifs.commit_pending(&reg, ids[1]),
            Err(InterfaceError::AbiMismatch)
        );
        // 合法的那条也被原子丢弃：新接口不存在。
        assert_eq!(
            ifs.bind(&reg, b"brand_new", InterfaceKind::Service, ABI_A),
            Err(InterfaceError::UnknownInterface)
        );
    }

    #[test]
    fn provider_liveness_revalidated_on_bind() {
        let (reg, ids) = ready_world();
        let mut ifs = InterfaceRegistry::new();
        publish_ready(
            &reg,
            &mut ifs,
            ids[0],
            b"sample",
            InterfaceKind::Service,
            ABI_A,
            ctx(1),
        );
        // provider 不在（另一个）registry 中 → bind 的存活复验必须拒绝。
        let empty = Registry::new();
        assert_eq!(
            ifs.bind(&empty, b"sample", InterfaceKind::Service, ABI_A),
            Err(InterfaceError::ProviderNotFound)
        );
    }

    // -- 6. 极小 function table：逻辑 binding 全链路 ------------------------

    #[test]
    fn consumer_calls_through_logical_binding_function_table() {
        let (reg, ids) = ready_world();
        let mut ifs = InterfaceRegistry::new();
        let mut counter = 10u32;
        let binding = publish_ready(
            &reg,
            &mut ifs,
            ids[0],
            b"sample",
            InterfaceKind::Service,
            ABI_A,
            &mut counter as *mut u32 as *mut (),
        );

        let view = ifs
            .bind(&reg, b"sample", InterfaceKind::Service, ABI_A)
            .unwrap();
        assert_eq!(view.id, binding);
        // consumer 把 opaque api cast 回自己声明的 function table 布局并调用。
        let vtable = unsafe { &*(view.api.cast::<SampleService>()) };
        let result = (vtable.do_thing)(view.ctx, 5);
        assert_eq!(
            result, 15,
            "direct call 通过逻辑 binding 抵达 provider 实现"
        );
        assert_eq!(view.generation, 1);
    }
}
