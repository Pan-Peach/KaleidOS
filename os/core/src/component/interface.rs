//! Component Interface Registry —— 组件→组件依赖的唯一机制（**骨架**）。
//!
//! # 规则（本轮定案）
//!
//! ```text
//! Component → Core    = Core Export ABI（component/export.rs，ELF undefined symbol）
//! Component → Component = Interface binding（本模块）——禁止 flat ELF symbol 互链
//! ```
//!
//! 组件替换的成立条件：consumer 拿的是**逻辑 binding**（`BindingView`），不是
//! "永不变更的 provider ELF 符号地址"。provider 换成新实现时，consumer 只需
//! 重新 `resolve_by_id` 拿新 `context`，**不需要 ELF reload**。
//!
//! # 数据模型
//!
//! - `InterfaceId`：Core 分配的接口身份（每个唯一接口名一条记录）。
//! - `BindingId`：一次 publish 产生的绑定槽；unbind/rebind **不失效**——
//!   unbind 只清 provider，槽位保留（consumer 持有的 id 继续有效）。
//! - `InterfaceKind`：Device / Service / Policy（复用文档 §2 的分类）。
//! - provider state：`provider: Option<ComponentId>`（None ⇔ Unbound）。
//! - `context: *mut ()`：provider 提供的 opaque 函数表/上下文。阶段一
//!   KernelNative 用 versioned function table（`#[repr(C)]` vtable，见下方示例）；
//!   未来可换 IPC stub / Wasm host call —— **binding 不含传输假设**。
//!
//! # 阶段一调用方式（KernelNative，direct call）
//!
//! ```rust
//! #[repr(C)]
//! struct SampleServiceV1 {
//!     abi_version: u32,
//!     ctx: *mut (),
//!     do_thing: extern "C" fn(*mut (), u32) -> u32,
//! }
//! // consumer: let view = interfaces.resolve(&reg, b"sample", Kind::Service, Ver(1))?;
//! //           let vtable = unsafe { &*(view.context.cast::<SampleServiceV1>()) };
//! ```
//!
//! # 下一阶段（seam）
//!
//! - compatible range（major/minor 版本区间）替代 exact match；
//! - 组件失败/卸载时 Core 自动 `unbind_provider`（本轮提供原语，接线留给
//!   ComponentManager）；
//! - 传输升级（IPC / Wasm host call）不改 binding 数据模型。

use alloc::vec::Vec;

use crate::component::registry::Registry;
use crate::component::{ComponentId, ComponentState};
use spin::{Mutex, Once};

/// 接口领域分类（与 docs/component-model.md §2 一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterfaceKind {
    Device,
    Service,
    Policy,
}

/// 接口版本（阶段一 exact match；compatible range 留下一阶段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct InterfaceVersion(u32);

impl InterfaceVersion {
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u32 {
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

/// 一次 resolve 返回的逻辑 binding 视图（Copy，无借用）。
/// consumer 把 `context` cast 成自己声明的 vtable 布局。
#[derive(Debug, Clone, Copy)]
pub struct BindingView {
    pub id: BindingId,
    pub interface: InterfaceId,
    pub version: InterfaceVersion,
    pub provider: ComponentId,
    /// opaque provider 函数表/上下文指针；消费方按接口契约 cast。
    /// 该指针的生命周期：provider Ready 期间有效；unbind/组件失败后必须
    /// 视为悬垂——consumer 应通过 `resolve_by_id` 重新获取。
    pub context: *mut (),
}

impl PartialEq for BindingView {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.interface == other.interface
            && self.version == other.version
            && self.provider == other.provider
            && core::ptr::eq(self.context, other.context)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterfaceError {
    /// publish：provider 不在组件注册表。
    ProviderNotFound,
    /// publish：provider 未进入 Ready（只能由已就绪组件提供接口）。
    ProviderNotReady,
    /// resolve：接口名未知。
    UnknownInterface,
    /// resolve：同接口名已用不同 kind 发布（一个名字一个 kind）。
    KindMismatch,
    /// resolve：版本不匹配（阶段一 exact match）。
    VersionMismatch,
    /// resolve：绑定槽存在但 provider 已 unbind。
    Unbound,
    /// resolve_by_id：无效槽 id。
    BindingNotFound,
    /// InterfaceId 空间耗尽（u32 单调递增）。
    IdExhausted,
}

/// 一条接口记录（按名字唯一）。
struct InterfaceRecord {
    id: InterfaceId,
    name: Vec<u8>,
    kind: InterfaceKind,
}

/// 一个绑定槽：interface + 当前 provider/版本/上下文。
/// `provider == None` ⇔ Unbound（槽保留，供重绑，binding id 不失效）。
struct BindingRecord {
    interface_id: InterfaceId,
    version: InterfaceVersion,
    provider: Option<ComponentId>,
    context: *mut (),
}

// `context` 是 opaque provider 句柄：Registry 只存取、永不解引用。
// Send/Sync 安全（与 export.rs 的 ExportAddress 同一理由；跨线程使用由
// 外层 Mutex 串行化）。
unsafe impl Send for BindingRecord {}
unsafe impl Sync for BindingRecord {}

/// 组件→组件 依赖的 Core 真相：谁提供了什么接口、当前绑到谁。
pub struct InterfaceRegistry {
    interfaces: Vec<InterfaceRecord>,
    bindings: Vec<BindingRecord>,
    next_interface_id: u32,
}

impl InterfaceRegistry {
    pub fn new() -> Self {
        Self {
            interfaces: Vec::new(),
            bindings: Vec::new(),
            next_interface_id: 1,
        }
    }

    /// 发布/提供接口。Core 校验：provider 必须存在且处于 Ready。
    /// 同接口名再次 publish = 重绑（新 provider/版本/上下文），
    /// 已有 binding 槽复用 → consumer 的 `BindingId` 不变（无需 reload）。
    pub fn publish(
        &mut self,
        components: &Registry,
        provider: ComponentId,
        name: &[u8],
        kind: InterfaceKind,
        version: InterfaceVersion,
        context: *mut (),
    ) -> Result<BindingId, InterfaceError> {
        let rec = components
            .get(provider)
            .ok_or(InterfaceError::ProviderNotFound)?;
        if rec.state != ComponentState::Ready {
            return Err(InterfaceError::ProviderNotReady);
        }

        let interface_id = match self.interfaces.iter().find(|r| r.name == name) {
            Some(record) => {
                if record.kind != kind {
                    return Err(InterfaceError::KindMismatch);
                }
                record.id
            }
            None => {
                let id = InterfaceId::from_raw(self.next_interface_id);
                self.next_interface_id = self
                    .next_interface_id
                    .checked_add(1)
                    .ok_or(InterfaceError::IdExhausted)?;
                self.interfaces.push(InterfaceRecord {
                    id,
                    name: name.to_vec(),
                    kind,
                });
                id
            }
        };

        // 复用已有绑定槽（重绑），否则新建。
        let index = self
            .bindings
            .iter()
            .position(|b| b.interface_id == interface_id);
        let binding_id = match index {
            Some(i) => {
                self.bindings[i].provider = Some(provider);
                self.bindings[i].version = version;
                self.bindings[i].context = context;
                BindingId::from_raw(u32::try_from(i).map_err(|_| InterfaceError::IdExhausted)?)
            }
            None => {
                let id = BindingId::from_raw(
                    u32::try_from(self.bindings.len()).map_err(|_| InterfaceError::IdExhausted)?,
                );
                self.bindings.push(BindingRecord {
                    interface_id,
                    version,
                    provider: Some(provider),
                    context,
                });
                id
            }
        };
        Ok(binding_id)
    }

    /// 按名解析 + 获取逻辑 binding。Core 再次验证 provider 仍存活
    /// （存在且 Ready）——组件卸载后 binding 立即不可用，不留给 consumer 悬垂调用。
    pub fn resolve(
        &self,
        components: &Registry,
        name: &[u8],
        kind: InterfaceKind,
        version: InterfaceVersion,
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
        if self.bindings[index].version != version {
            return Err(InterfaceError::VersionMismatch);
        }
        self.view_of(components, index)
    }

    /// 按槽 id 重新获取 binding（provider 替换后 consumer 无需 ELF reload）。
    /// 版本由消费方从返回视图自行校验兼容性。
    pub fn resolve_by_id(
        &self,
        components: &Registry,
        id: BindingId,
    ) -> Result<BindingView, InterfaceError> {
        if id.raw() as usize >= self.bindings.len() {
            return Err(InterfaceError::BindingNotFound);
        }
        self.view_of(components, id.raw() as usize)
    }

    /// 生成视图，并做 provider 存活二次校验（Core 验证后才交付）。
    fn view_of(&self, components: &Registry, index: usize) -> Result<BindingView, InterfaceError> {
        let binding = &self.bindings[index];
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
            version: binding.version,
            provider,
            context: binding.context,
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
        binding.context = core::ptr::null_mut();
        Ok(())
    }

    /// 解除某 provider 的全部绑定（组件失败/卸载时由 Core 调用；接线留给
    /// ComponentManager，本轮提供原语）。
    pub fn unbind_provider(&mut self, provider: ComponentId) {
        for binding in &mut self.bindings {
            if binding.provider == Some(provider) {
                binding.provider = None;
                binding.context = core::ptr::null_mut();
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
    use crate::component::registry::Registry;

    const V1: InterfaceVersion = InterfaceVersion::from_raw(1);
    const V2: InterfaceVersion = InterfaceVersion::from_raw(2);

    /// 测试用 context：真实局部地址（区分 provider 即可，生命周期限测试函数内）。
    fn ctx(n: u32) -> *mut () {
        let mut slot = n;
        &mut slot as *mut u32 as *mut ()
    }

    /// 构造注册表并声明两个 Ready 组件（provider_a / provider_b / consumer）。
    fn ready_world() -> (Registry, Vec<ComponentId>) {
        let mut reg = Registry::new();
        let mut ids = Vec::new();
        for name in [&b"provider_a"[..], &b"provider_b"[..], &b"consumer"[..]] {
            let id = reg.declare(name, 1, 2, None).unwrap();
            reg.resolve(id).unwrap();
            reg.start(id).unwrap();
            ids.push(id);
        }
        (reg, ids)
    }

    // -- 1. provider publish -----------------------------------------------

    #[test]
    fn publish_and_resolve_roundtrip() {
        let (reg, ids) = ready_world();
        let mut interfaces = InterfaceRegistry::new();
        let ctx = 0x1234usize as *mut ();
        let binding = interfaces
            .publish(&reg, ids[0], b"sample", InterfaceKind::Service, V1, ctx)
            .unwrap();

        let view = interfaces
            .resolve(&reg, b"sample", InterfaceKind::Service, V1)
            .unwrap();
        assert_eq!(view.id, binding);
        assert_eq!(view.provider, ids[0]);
        assert_eq!(view.version, V1);
        assert_eq!(view.context, ctx);
    }

    #[test]
    fn publish_rejects_unknown_provider() {
        let (reg, _ids) = ready_world();
        let mut interfaces = InterfaceRegistry::new();
        assert_eq!(
            interfaces.publish(
                &reg,
                ComponentId::from_raw(99),
                b"sample",
                InterfaceKind::Service,
                V1,
                core::ptr::null_mut(),
            ),
            Err(InterfaceError::ProviderNotFound)
        );
    }

    #[test]
    fn publish_rejects_non_ready_provider() {
        let mut reg = Registry::new();
        let declared = reg.declare(b"not_ready", 1, 2, None).unwrap();
        // 不 resolve/start：保持 Declared
        let mut interfaces = InterfaceRegistry::new();
        assert_eq!(
            interfaces.publish(
                &reg,
                declared,
                b"sample",
                InterfaceKind::Service,
                V1,
                core::ptr::null_mut(),
            ),
            Err(InterfaceError::ProviderNotReady)
        );
    }

    #[test]
    fn same_name_different_kind_is_rejected() {
        let (reg, ids) = ready_world();
        let mut interfaces = InterfaceRegistry::new();
        interfaces
            .publish(&reg, ids[0], b"sample", InterfaceKind::Service, V1, ctx(1))
            .unwrap();
        assert_eq!(
            interfaces.publish(&reg, ids[0], b"sample", InterfaceKind::Device, V1, ctx(2)),
            Err(InterfaceError::KindMismatch)
        );
    }

    // -- 2. consumer resolve + 3. version mismatch --------------------------

    #[test]
    fn resolve_unknown_interface_is_rejected() {
        let (reg, _ids) = ready_world();
        let interfaces = InterfaceRegistry::new();
        assert_eq!(
            interfaces.resolve(&reg, b"nope", InterfaceKind::Service, V1),
            Err(InterfaceError::UnknownInterface)
        );
    }

    #[test]
    fn resolve_version_mismatch_is_rejected() {
        let (reg, ids) = ready_world();
        let mut interfaces = InterfaceRegistry::new();
        interfaces
            .publish(&reg, ids[0], b"sample", InterfaceKind::Service, V1, ctx(1))
            .unwrap();
        assert_eq!(
            interfaces.resolve(&reg, b"sample", InterfaceKind::Service, V2),
            Err(InterfaceError::VersionMismatch)
        );
    }

    // -- 4. unbind 后 binding 不可用 ---------------------------------------

    #[test]
    fn unbind_makes_binding_unavailable() {
        let (reg, ids) = ready_world();
        let mut interfaces = InterfaceRegistry::new();
        let binding = interfaces
            .publish(&reg, ids[0], b"sample", InterfaceKind::Service, V1, ctx(1))
            .unwrap();
        interfaces.unbind(binding).unwrap();
        assert_eq!(
            interfaces.resolve(&reg, b"sample", InterfaceKind::Service, V1),
            Err(InterfaceError::Unbound)
        );
        assert_eq!(
            interfaces.resolve_by_id(&reg, binding),
            Err(InterfaceError::Unbound)
        );
    }

    #[test]
    fn unbind_unknown_binding_is_not_found() {
        let mut interfaces = InterfaceRegistry::new();
        assert_eq!(
            interfaces.unbind(BindingId::from_raw(7)),
            Err(InterfaceError::BindingNotFound)
        );
    }

    #[test]
    fn unbind_provider_revokes_all_its_bindings() {
        let (reg, ids) = ready_world();
        let mut interfaces = InterfaceRegistry::new();
        interfaces
            .publish(&reg, ids[0], b"svc_a", InterfaceKind::Service, V1, ctx(1))
            .unwrap();
        interfaces
            .publish(&reg, ids[0], b"svc_b", InterfaceKind::Service, V1, ctx(2))
            .unwrap();
        interfaces.unbind_provider(ids[0]);
        assert_eq!(interfaces.binding_count(), 2, "槽保留");
        assert_eq!(
            interfaces.resolve(&reg, b"svc_a", InterfaceKind::Service, V1),
            Err(InterfaceError::Unbound)
        );
        assert_eq!(
            interfaces.resolve(&reg, b"svc_b", InterfaceKind::Service, V1),
            Err(InterfaceError::Unbound)
        );
        // 其他 provider 的绑定不受影响
        interfaces
            .publish(&reg, ids[1], b"svc_a", InterfaceKind::Service, V1, ctx(3))
            .unwrap();
        assert!(
            interfaces
                .resolve(&reg, b"svc_a", InterfaceKind::Service, V1)
                .is_ok()
        );
    }

    // -- 5. 重绑新 provider：consumer 不需要 ELF reload ----------------------

    #[test]
    fn rebind_new_provider_keeps_consumer_binding_id() {
        let (reg, ids) = ready_world();
        let mut interfaces = InterfaceRegistry::new();
        let binding = interfaces
            .publish(&reg, ids[0], b"sample", InterfaceKind::Service, V1, ctx(1))
            .unwrap();

        // provider_a 卸载 → Core 解绑
        interfaces.unbind(binding).unwrap();

        // provider_b 重绑同一接口（新版本、新上下文）
        let rebound = interfaces
            .publish(&reg, ids[1], b"sample", InterfaceKind::Service, V2, ctx(2))
            .unwrap();
        assert_eq!(rebound, binding, "槽复用：consumer 持有的 id 不变");

        // consumer 用旧 id 重新获取 → 拿到新 provider 的上下文（无 ELF reload）
        let view = interfaces.resolve_by_id(&reg, binding).unwrap();
        assert_eq!(view.provider, ids[1]);
        assert_eq!(view.version, V2);
        assert_eq!(view.context, ctx(2));
    }

    // -- 6. 极小 vtable 接口：逻辑 binding 全链路 ---------------------------

    /// 测试用极小接口（示例性 vtable 布局，`#[repr(C)]`，versioned）。
    #[repr(C)]
    struct SampleServiceV1 {
        abi_version: u32,
        ctx: *mut (),
        do_thing: extern "C" fn(*mut (), u32) -> u32,
    }

    extern "C" fn sample_impl(ctx: *mut (), input: u32) -> u32 {
        // provider 侧实现：ctx 指向一个计数器
        let counter = unsafe { &mut *(ctx as *mut u32) };
        *counter = counter.wrapping_add(input);
        *counter
    }

    #[test]
    fn consumer_calls_through_logical_binding_vtable() {
        let (reg, ids) = ready_world();
        let mut interfaces = InterfaceRegistry::new();
        let mut counter = 10u32;
        let vtable = SampleServiceV1 {
            abi_version: 1,
            ctx: &mut counter as *mut u32 as *mut (),
            do_thing: sample_impl,
        };
        let binding = interfaces
            .publish(
                &reg,
                ids[0],
                b"sample",
                InterfaceKind::Service,
                V1,
                (&vtable as *const SampleServiceV1).cast_mut().cast(),
            )
            .unwrap();

        let view = interfaces
            .resolve(&reg, b"sample", InterfaceKind::Service, V1)
            .unwrap();
        assert_eq!(view.id, binding);
        // consumer 把 opaque context cast 回自己声明的 vtable 布局并调用
        let got = unsafe { &*(view.context.cast::<SampleServiceV1>()) };
        assert_eq!(got.abi_version, 1);
        let result = (got.do_thing)(got.ctx, 5);
        assert_eq!(
            result, 15,
            "direct call 通过逻辑 binding 抵达 provider 实现"
        );
    }

    #[test]
    fn provider_liveness_revalidated_on_resolve() {
        let (reg, ids) = ready_world();
        let mut interfaces = InterfaceRegistry::new();
        interfaces
            .publish(&reg, ids[0], b"sample", InterfaceKind::Service, V1, ctx(1))
            .unwrap();
        // provider 被卸载（从组件注册表移除）→ resolve 必须拒绝
        let mut reg = reg;
        reg.unload(ids[0]).unwrap();
        assert_eq!(
            interfaces.resolve(&reg, b"sample", InterfaceKind::Service, V1),
            Err(InterfaceError::ProviderNotFound)
        );
    }
}
