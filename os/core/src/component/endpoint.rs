//! Component Endpoint Registry —— Contract / Endpoint 模型（**Phase A：Core 侧真相骨架**）。
//!
//! # 定位
//!
//! `component/interface.rs` 是上一代模型：**一个全局接口名 → 一个 provider 绑定槽**
//! （`api` / `ctx` function table），同 ABI 重发布是**覆盖**。本模块是它的替代真相模型；
//! 本阶段只落地 Core 侧数据模型，**不改 ABI、不导出、不迁 consumer**——
//! `interface.rs` 保持原样（scheduler / core_test 仍在用），下一阶段整体替换。
//!
//! ```text
//! Contract：契约身份（kind + exact ABI fingerprint + 诊断名）—— 语义
//! Endpoint：某个组件实例在某个端口名上的一次发布 —— 存在
//! ```
//!
//! # 规则
//!
//! - **Contract ≠ Endpoint**：多个 provider 可以实现同一契约。契约由**首次发布**
//!   建立 `kind` / `abi`（[`ContractRecord`]），后续发布 kind / abi 不一致必须拒绝。
//! - **Endpoint 归属唯一**：`ComponentId` 就是实例身份；endpoint 生命周期内 owner
//!   不可变。两个实例可以发布**同名端口 + 同一契约**，各自持有不同 endpoint。
//! - **publish 创建新 endpoint，绝不覆盖**：没有"同 ABI 覆盖原槽"。端口名在
//!   **provider 实例内唯一**：重复发布同一端口名（含已失效名字）拒绝，不重定向。
//! - **consumer 只持有 [`EndpointId`]**：本模型没有 `api` / `ctx`——provider callable
//!   指针不进入 Core 真相；传输方式（direct call / IPC / Wasm host call）由未来
//!   阶段按执行域决定，不改本数据模型。
//! - **EndpointId 单调、从 1 起、绝不回收 / 重定向**：provider 停止或失败后旧
//!   endpoint 永久 `Invalid`，绝不会解析到新实例。
//! - 标识符不带版本后缀：契约演进 = 原地替换（`AGENTS.md`）。
//!
//! # 表结构（真相 / 发现分离）
//!
//! - `endpoints`：endpoint 真相（[`EndpointRecord`] 是 `Copy`：无 `Vec`、无借用，
//!   `lookup` 直接返回值）。
//! - `contracts`：契约身份（首次发布建立，之后只校验）。
//! - `names`：发现表 `(provider, 端口名) → endpoint`（[`EndpointName`]）。
//!   单独成表，`lookup` 不被名字检索拖慢；只有 `discover` 查它。
//! - `pending`：staged publish 暂存。
//!
//! # Staged publish（与 `interface.rs` 同形）
//!
//! `kcomp_instance_create()` 执行期间发布**不立即创建 endpoint**：
//! [`EndpointRegistry::stage_publish`] 只记录 pending（此时校验 provider 存在且
//! `Starting | Ready`）；Core 在 create 返回 0 后调用
//! [`EndpointRegistry::commit_pending`] **原子提交**：
//!
//! 1. 整体校验（契约 kind / abi、端口名唯一、id 容量），任一失败 → **整批丢弃**，
//!    已有 endpoint 完全不受影响；
//! 2. 通过后逐条**创建新 endpoint**（`Live`）并登记发现名。
//!
//! init 失败 / panic：[`EndpointRegistry::discard_pending`] 丢弃该实例全部 pending。
//!
//! # 本阶段不做（seam / TODO）
//!
//! - 不导出 `kcore_*`、不改组件 ABI、不迁 consumer；
//! - 不新增 `TraceEvent`（事件 kind 是 ABI 编码，留给下一阶段）；
//! - 不做 endpoint 回收（`Invalid` 记录保留为 tombstone，id 不复用）。

use alloc::vec::Vec;

use crate::component::interface::InterfaceAbi;
use crate::component::registry::Registry;
use crate::component::{ComponentId, ComponentState};

pub use crate::generated::abi::InterfaceKind;

/// 契约身份（如 `block.device`）：Core / 组合策略提供的不透明 id。
///
/// 多个 provider 可以实现同一契约；契约由**首次发布**建立 `kind` / `abi`
/// （见 [`ContractRecord`]），后续发布 kind / abi 不一致必须拒绝。
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContractId(u64);

impl ContractId {
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Endpoint 身份：consumer 持有的**唯一** handle。
///
/// Core 分配的单调 id（从 1 起、绝不回收、绝不重定向）；provider 停止 / 失败后
/// 旧 endpoint 永久 `Invalid`，绝不会解析到新实例。
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EndpointId(u64);

impl EndpointId {
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Endpoint 生命周期状态。
///
/// `Pending` 预留给"已预留 id、尚未提交"的未来路径；当前
/// [`EndpointRegistry::stage_publish`] 不创建 endpoint，
/// [`EndpointRegistry::commit_pending`] 直接产出 `Live`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointState {
    Pending,
    Live,
    Invalid,
}

/// 一条 endpoint 真相（`Copy`：无 `Vec`、无借用、无 provider callable 指针）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointRecord {
    pub id: EndpointId,
    /// 拥有该 endpoint 的组件实例（`ComponentId` 即实例身份）；生命周期内不可变。
    pub owner: ComponentId,
    /// provider 定义的不透明 dispatch token；Core 从不解释。
    pub port: u32,
    pub contract: ContractId,
    /// exact ABI fingerprint（发布时从契约记录拷贝）。
    pub abi: InterfaceAbi,
    pub state: EndpointState,
}

/// 一条契约记录（按 [`ContractId`] 唯一）：首次发布建立 `kind` / `abi`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractRecord {
    pub id: ContractId,
    /// 诊断标签：首次建立该契约的发布所用端口名（契约身份 = id + kind + abi）。
    pub name: Vec<u8>,
    pub kind: InterfaceKind,
    pub abi: InterfaceAbi,
}

/// 发现表记录：`(provider, 端口名) → endpoint`。
///
/// 与 `endpoints` 分表，`lookup` 走 id 查找（返回 `Copy`），不被名字检索拖慢；
/// 端口名只要求**在 provider 实例内唯一**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointName {
    pub provider: ComponentId,
    pub name: Vec<u8>,
    pub contract: ContractId,
    pub endpoint: EndpointId,
}

/// 一次 staged publish（仅暂存；commit 成功才产出 endpoint）。
struct PendingPublication {
    provider: ComponentId,
    port_name: Vec<u8>,
    contract: ContractId,
    kind: InterfaceKind,
    abi: InterfaceAbi,
    port: u32,
}

/// Endpoint 模型的拒绝原因（独立于 [`crate::component::interface::InterfaceError`]：
/// 两代模型的概念不同，不共用错误类型）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointError {
    /// lookup / discover：endpoint id 未知。
    EndpointNotFound,
    /// lookup / discover：endpoint 已 `Invalid`（永不交付死 endpoint）。
    EndpointDead,
    /// stage / commit：provider 不在组件注册表；lookup / discover：owner 记录已不存在。
    ProviderNotFound,
    /// stage / commit：provider 不在可发布状态（`Starting` / `Ready` 之外）。
    ProviderNotReady,
    /// lookup / discover：传入 contract 与记录不符。
    ContractMismatch,
    /// commit：同契约已用不同 kind 建立（kind 由首次发布唯一确定）。
    KindMismatch,
    /// commit / lookup：ABI fingerprint 不一致（exact match 失败）。
    AbiMismatch,
    /// commit：同一 provider 实例的端口名重复（名字在实例内唯一，不重定向）。
    DuplicatePort,
    /// EndpointId 空间耗尽（u64 单调递增）。
    IdExhausted,
}

/// Contract / Endpoint 的 Core 真相：谁在哪个端口上发布了哪个契约。
pub struct EndpointRegistry {
    endpoints: Vec<EndpointRecord>,
    contracts: Vec<ContractRecord>,
    names: Vec<EndpointName>,
    pending: Vec<PendingPublication>,
    next_endpoint_id: u64,
}

impl EndpointRegistry {
    pub fn new() -> Self {
        Self {
            endpoints: Vec::new(),
            contracts: Vec::new(),
            names: Vec::new(),
            pending: Vec::new(),
            next_endpoint_id: 1,
        }
    }

    /// **Staged publish**：记录一条 pending publication，不创建 endpoint。
    ///
    /// 由 `kcore_*_publish` 在 `kcomp_instance_create()` 执行期间调用（provider 此时
    /// 处于 `Starting`）。Core 只校验 provider 存在且处于可发布状态；真正的契约
    /// 冲突 / 端口名冲突 / id 容量在 [`Self::commit_pending`] 统一判定（原子语义）。
    #[allow(clippy::too_many_arguments)]
    pub fn stage_publish(
        &mut self,
        components: &Registry,
        provider: ComponentId,
        port_name: &[u8],
        contract: ContractId,
        kind: InterfaceKind,
        abi: InterfaceAbi,
        port: u32,
    ) -> Result<(), EndpointError> {
        let record = components
            .get(provider)
            .ok_or(EndpointError::ProviderNotFound)?;
        // `Starting` = 正在 call_create；`Ready` 允许未来 monitor 驱动的重发布。
        if !matches!(
            record.state,
            ComponentState::Starting | ComponentState::Ready
        ) {
            return Err(EndpointError::ProviderNotReady);
        }
        self.pending.push(PendingPublication {
            provider,
            port_name: port_name.to_vec(),
            contract,
            kind,
            abi,
            port,
        });
        Ok(())
    }

    /// 提交某 provider 的全部 pending publications（`kcomp_instance_create()` 返回 0
    /// 后由 Core 调用）。
    ///
    /// **原子语义**：先整体校验（契约 kind / abi、端口名唯一、id 容量），任一失败
    /// 则丢弃该 provider 全部 pending 并返回 `Err`——已有 endpoint 完全不受影响；
    /// 校验通过后一次性创建**全新** endpoint（绝不覆盖）并登记发现名。
    pub fn commit_pending(
        &mut self,
        components: &Registry,
        provider: ComponentId,
    ) -> Result<(), EndpointError> {
        // 仅取出该 provider 的 pending（其它 provider 原样保留）。
        let mut staging = Vec::new();
        self.pending.retain(|p| {
            if p.provider == provider {
                staging.push(PendingPublication {
                    provider: p.provider,
                    port_name: p.port_name.clone(),
                    contract: p.contract,
                    kind: p.kind,
                    abi: p.abi,
                    port: p.port,
                });
                false
            } else {
                true
            }
        });

        // provider 必须仍存在且可发布（create 成功路径下为 `Starting`）。
        let record = components
            .get(provider)
            .ok_or(EndpointError::ProviderNotFound)?;
        if !matches!(
            record.state,
            ComponentState::Starting | ComponentState::Ready
        ) {
            return Err(EndpointError::ProviderNotReady);
        }

        // 校验阶段：任一冲突 → 整批丢弃（staging 随作用域结束被 drop）。
        self.validate_staging(provider, &staging)?;

        // 应用阶段（校验已通过，不再有失败）：逐条创建新 endpoint + 发现名。
        for p in staging {
            if !self.contracts.iter().any(|c| c.id == p.contract) {
                // 首次发布建立契约身份；`name` 只是诊断标签。
                self.contracts.push(ContractRecord {
                    id: p.contract,
                    name: p.port_name.clone(),
                    kind: p.kind,
                    abi: p.abi,
                });
            }
            let id = EndpointId::from_raw(self.next_endpoint_id);
            self.next_endpoint_id += 1;
            self.endpoints.push(EndpointRecord {
                id,
                owner: p.provider,
                port: p.port,
                contract: p.contract,
                abi: p.abi,
                state: EndpointState::Live,
            });
            self.names.push(EndpointName {
                provider: p.provider,
                name: p.port_name,
                contract: p.contract,
                endpoint: id,
            });
        }
        Ok(())
    }

    /// 丢弃某 provider 的全部 pending publications（init 失败 / panic 路径）。
    /// 已有 endpoint 完全不受影响。
    pub fn discard_pending(&mut self, provider: ComponentId) {
        self.pending.retain(|p| p.provider != provider);
    }

    /// consumer 按 `EndpointId` 解析：校验 endpoint 存活 + owner 存活 + contract / abi
    /// 精确匹配，返回 `Copy` 记录（Core 验证后才交付；绝不交付死 endpoint）。
    pub fn lookup(
        &self,
        components: &Registry,
        id: EndpointId,
        contract: ContractId,
        abi: InterfaceAbi,
    ) -> Result<EndpointRecord, EndpointError> {
        let record = *self
            .endpoints
            .iter()
            .find(|r| r.id == id)
            .ok_or(EndpointError::EndpointNotFound)?;
        if record.state != EndpointState::Live {
            return Err(EndpointError::EndpointDead);
        }
        if record.contract != contract {
            return Err(EndpointError::ContractMismatch);
        }
        if record.abi != abi {
            return Err(EndpointError::AbiMismatch);
        }
        check_owner_live(components, record.owner)?;
        Ok(record)
    }

    /// 组合期显式解析：composer 问 `(ComponentId, 端口名, contract) → EndpointId`。
    ///
    /// 名字表按 provider 隔离；返回前同样做存活校验（不交付死 endpoint）。
    pub fn discover(
        &self,
        components: &Registry,
        provider: ComponentId,
        port_name: &[u8],
        contract: ContractId,
    ) -> Result<EndpointId, EndpointError> {
        let name = self
            .names
            .iter()
            .find(|n| n.provider == provider && n.name == port_name)
            .ok_or(EndpointError::EndpointNotFound)?;
        if name.contract != contract {
            return Err(EndpointError::ContractMismatch);
        }
        let record = self
            .endpoints
            .iter()
            .find(|r| r.id == name.endpoint)
            .ok_or(EndpointError::EndpointNotFound)?;
        if record.state != EndpointState::Live {
            return Err(EndpointError::EndpointDead);
        }
        check_owner_live(components, record.owner)?;
        Ok(record.id)
    }

    /// 使一个 endpoint 永久失效（provider 停止 / 失败路径）。
    /// 幂等；未知 id 静默 no-op（调用方不持有 endpoint 生命周期的完整视图）。
    pub fn invalidate_endpoint(&mut self, id: EndpointId) {
        if let Some(record) = self.endpoints.iter_mut().find(|r| r.id == id) {
            record.state = EndpointState::Invalid;
        }
    }

    /// 使某 provider 的全部 endpoint 永久失效（组件失败 / 卸载时由 Core 调用）。
    /// 其它 provider 的 endpoint 不受影响。
    pub fn invalidate_provider(&mut self, provider: ComponentId) {
        for record in &mut self.endpoints {
            if record.owner == provider {
                record.state = EndpointState::Invalid;
            }
        }
    }

    pub fn endpoint_count(&self) -> usize {
        self.endpoints.len()
    }

    pub fn live_count(&self) -> usize {
        self.endpoints
            .iter()
            .filter(|r| r.state == EndpointState::Live)
            .count()
    }

    /// 只读校验一个已取出的批（不改真相）：
    ///
    /// - **契约**：批内首次出现的契约在本地 shadow 中建立 kind / abi，同批后续
    ///   发布同样受校验（"首次发布建立唯一真相"在批内也成立）；
    /// - **端口名**：批内重复 + 与既有名字记录（含 Invalid tombstone）冲突都拒绝；
    /// - **id 容量**：一次性预留整批，不足则整批失败。
    fn validate_staging(
        &self,
        provider: ComponentId,
        staging: &[PendingPublication],
    ) -> Result<(), EndpointError> {
        for (index, p) in staging.iter().enumerate() {
            let established = self
                .contracts
                .iter()
                .find(|c| c.id == p.contract)
                .map(|c| (c.kind, c.abi))
                .or_else(|| {
                    staging[..index]
                        .iter()
                        .find(|q| q.contract == p.contract)
                        .map(|q| (q.kind, q.abi))
                });
            if let Some((kind, abi)) = established {
                if kind != p.kind {
                    return Err(EndpointError::KindMismatch);
                }
                if abi != p.abi {
                    return Err(EndpointError::AbiMismatch);
                }
            }
            let duplicate = staging[..index].iter().any(|q| q.port_name == p.port_name)
                || self
                    .names
                    .iter()
                    .any(|n| n.provider == provider && n.name == p.port_name);
            if duplicate {
                return Err(EndpointError::DuplicatePort);
            }
        }
        self.next_endpoint_id
            .checked_add(staging.len() as u64)
            .ok_or(EndpointError::IdExhausted)?;
        Ok(())
    }
}

impl Default for EndpointRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// owner 存活二次校验（Core 验证后才交付；`lookup` / `discover` 共用）。
///
/// - 实例记录已不存在（身份消失）→ `ProviderNotFound`；
/// - 存在但不在 `Ready`（`Stopping` / `Stopped` / `Failed` / 未完成 init）→ `EndpointDead`。
fn check_owner_live(components: &Registry, owner: ComponentId) -> Result<(), EndpointError> {
    let record = components
        .get(owner)
        .ok_or(EndpointError::ProviderNotFound)?;
    if record.state != ComponentState::Ready {
        return Err(EndpointError::EndpointDead);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::image::ComponentImageId;
    use crate::component::registry::{Registry, RegistryError};
    use alloc::vec::Vec;

    /// 测试用镜像身份：registry 只把它当身份键（image 表是另一份真相）。
    const IMAGE: ComponentImageId = ComponentImageId::from_raw(1);

    const CONTRACT: ContractId = ContractId::from_raw(0xC0DE_0001);
    const OTHER_CONTRACT: ContractId = ContractId::from_raw(0xC0DE_0002);
    const ABI_A: InterfaceAbi = InterfaceAbi::from_raw(0xAAAA_0001);
    const ABI_B: InterfaceAbi = InterfaceAbi::from_raw(0xBBBB_0002);

    /// 构造注册表并声明三个 Ready 实例（provider_a / provider_b / provider_c）。
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

    /// 发布一个端口并提交（provider 已 Ready），返回 `discover` 解析到的 endpoint。
    fn publish_ready(
        er: &mut EndpointRegistry,
        reg: &Registry,
        provider: ComponentId,
        port_name: &[u8],
        contract: ContractId,
        port: u32,
    ) -> EndpointId {
        er.stage_publish(
            reg,
            provider,
            port_name,
            contract,
            InterfaceKind::Device,
            ABI_A,
            port,
        )
        .unwrap();
        er.commit_pending(reg, provider).unwrap();
        er.discover(reg, provider, port_name, contract).unwrap()
    }

    // -- 1. staged publish + commit ----------------------------------------

    #[test]
    fn staged_publish_creates_no_endpoint_until_commit() {
        // Given：一个 Ready provider 与一次 staged publish。
        let (reg, ids) = ready_world();
        let mut er = EndpointRegistry::new();
        er.stage_publish(
            &reg,
            ids[0],
            b"blk0",
            CONTRACT,
            InterfaceKind::Device,
            ABI_A,
            7,
        )
        .unwrap();

        // Then：commit 之前不可见（不创建半成品 endpoint）。
        assert_eq!(er.endpoint_count(), 0);
        assert_eq!(er.live_count(), 0);
        assert_eq!(
            er.discover(&reg, ids[0], b"blk0", CONTRACT),
            Err(EndpointError::EndpointNotFound)
        );

        // When：commit。
        er.commit_pending(&reg, ids[0]).unwrap();

        // Then：恰好一条 Live endpoint，字段与发布一致。
        assert_eq!(er.endpoint_count(), 1);
        assert_eq!(er.live_count(), 1);
        let id = er.discover(&reg, ids[0], b"blk0", CONTRACT).unwrap();
        let record = er.lookup(&reg, id, CONTRACT, ABI_A).unwrap();
        assert_eq!(record.owner, ids[0]);
        assert_eq!(record.port, 7);
        assert_eq!(record.contract, CONTRACT);
        assert_eq!(record.abi, ABI_A);
        assert_eq!(record.state, EndpointState::Live);
    }

    #[test]
    fn stage_publish_rejects_unknown_and_non_starting_provider() {
        let (reg, _ids) = ready_world();
        let mut er = EndpointRegistry::new();
        assert_eq!(
            er.stage_publish(
                &reg,
                ComponentId::from_raw(99),
                b"blk0",
                CONTRACT,
                InterfaceKind::Device,
                ABI_A,
                0,
            ),
            Err(EndpointError::ProviderNotFound)
        );

        // Declared / Resolved 拒绝；Starting / Ready 接受（create 期发布 = Starting）。
        let mut state = Registry::new();
        let declared = state.declare(IMAGE).unwrap();
        assert_eq!(
            er.stage_publish(
                &state,
                declared,
                b"blk0",
                CONTRACT,
                InterfaceKind::Device,
                ABI_A,
                0,
            ),
            Err(EndpointError::ProviderNotReady)
        );
        state.resolve(declared).unwrap();
        assert_eq!(
            er.stage_publish(
                &state,
                declared,
                b"blk0",
                CONTRACT,
                InterfaceKind::Device,
                ABI_A,
                0,
            ),
            Err(EndpointError::ProviderNotReady)
        );
        state.begin_start(declared).unwrap();
        er.stage_publish(
            &state,
            declared,
            b"blk0",
            CONTRACT,
            InterfaceKind::Device,
            ABI_A,
            0,
        )
        .unwrap();
    }

    #[test]
    fn discard_pending_drops_staging_without_touching_existing() {
        let (reg, ids) = ready_world();
        let mut er = EndpointRegistry::new();
        let existing = publish_ready(&mut er, &reg, ids[0], b"blk0", CONTRACT, 7);
        er.stage_publish(
            &reg,
            ids[1],
            b"blk1",
            CONTRACT,
            InterfaceKind::Device,
            ABI_A,
            8,
        )
        .unwrap();
        er.discard_pending(ids[1]);

        // 已丢弃：commit 空批是 no-op；旧 endpoint 不受影响。
        assert_eq!(er.commit_pending(&reg, ids[1]), Ok(()));
        assert_eq!(er.endpoint_count(), 1);
        assert_eq!(
            er.lookup(&reg, existing, CONTRACT, ABI_A).unwrap().state,
            EndpointState::Live
        );
    }

    // -- 2. publish 创建新 endpoint：两个实例同名同契约 ----------------------

    #[test]
    fn same_port_from_two_instances_creates_two_live_endpoints() {
        // Given：两个 Ready 实例发布同名端口、同一契约（同 ABI）。
        let (reg, ids) = ready_world();
        let mut er = EndpointRegistry::new();
        let a = publish_ready(&mut er, &reg, ids[0], b"blk0", CONTRACT, 7);
        let b = publish_ready(&mut er, &reg, ids[1], b"blk0", CONTRACT, 7);

        // Then：两个不同 endpoint，都 Live，归属各自实例，谁也不覆盖谁。
        assert_ne!(a, b);
        assert_eq!(er.endpoint_count(), 2);
        assert_eq!(er.live_count(), 2);
        assert_eq!(er.lookup(&reg, a, CONTRACT, ABI_A).unwrap().owner, ids[0]);
        assert_eq!(er.lookup(&reg, b, CONTRACT, ABI_A).unwrap().owner, ids[1]);
        assert_eq!(
            er.lookup(&reg, a, CONTRACT, ABI_A).unwrap().state,
            EndpointState::Live
        );
        assert_eq!(
            er.lookup(&reg, b, CONTRACT, ABI_A).unwrap().state,
            EndpointState::Live
        );
    }

    // -- 3. commit 原子性 ----------------------------------------------------

    #[test]
    fn commit_conflict_creates_no_endpoints_from_the_batch() {
        let (reg, ids) = ready_world();
        let mut er = EndpointRegistry::new();
        let existing = publish_ready(&mut er, &reg, ids[0], b"blk0", CONTRACT, 7);

        // 批内两条：blk1 合法，blk2 与既有契约 ABI 冲突 → 整批丢弃。
        er.stage_publish(
            &reg,
            ids[1],
            b"blk1",
            CONTRACT,
            InterfaceKind::Device,
            ABI_A,
            8,
        )
        .unwrap();
        er.stage_publish(
            &reg,
            ids[1],
            b"blk2",
            CONTRACT,
            InterfaceKind::Device,
            ABI_B,
            9,
        )
        .unwrap();
        assert_eq!(
            er.commit_pending(&reg, ids[1]),
            Err(EndpointError::AbiMismatch)
        );
        assert_eq!(er.endpoint_count(), 1, "合法的那条也不得创建");
        assert_eq!(
            er.discover(&reg, ids[1], b"blk1", CONTRACT),
            Err(EndpointError::EndpointNotFound)
        );

        // kind 冲突同样整批丢弃。
        er.stage_publish(
            &reg,
            ids[1],
            b"blk3",
            CONTRACT,
            InterfaceKind::Device,
            ABI_A,
            10,
        )
        .unwrap();
        er.stage_publish(
            &reg,
            ids[1],
            b"blk4",
            CONTRACT,
            InterfaceKind::Service,
            ABI_A,
            11,
        )
        .unwrap();
        assert_eq!(
            er.commit_pending(&reg, ids[1]),
            Err(EndpointError::KindMismatch)
        );
        assert_eq!(er.endpoint_count(), 1);
        assert_eq!(
            er.discover(&reg, ids[1], b"blk3", CONTRACT),
            Err(EndpointError::EndpointNotFound)
        );

        // 全新契约在同一批内自相矛盾（首次发布建立唯一真相）→ 同样整批丢弃。
        er.stage_publish(
            &reg,
            ids[1],
            b"x1",
            OTHER_CONTRACT,
            InterfaceKind::Device,
            ABI_A,
            12,
        )
        .unwrap();
        er.stage_publish(
            &reg,
            ids[1],
            b"x2",
            OTHER_CONTRACT,
            InterfaceKind::Device,
            ABI_B,
            13,
        )
        .unwrap();
        assert_eq!(
            er.commit_pending(&reg, ids[1]),
            Err(EndpointError::AbiMismatch)
        );
        assert_eq!(er.endpoint_count(), 1);
        assert_eq!(
            er.discover(&reg, ids[1], b"x1", OTHER_CONTRACT),
            Err(EndpointError::EndpointNotFound)
        );

        // 旧 endpoint 完全不脏。
        assert_eq!(
            er.lookup(&reg, existing, CONTRACT, ABI_A).unwrap().owner,
            ids[0]
        );
    }

    #[test]
    fn commit_id_exhaustion_leaves_registry_untouched() {
        let (reg, ids) = ready_world();
        let mut er = EndpointRegistry::new();
        // 直接置位私有计数：u64 耗尽无法用真实调用序列走到。
        er.next_endpoint_id = u64::MAX;
        er.stage_publish(
            &reg,
            ids[0],
            b"blk0",
            CONTRACT,
            InterfaceKind::Device,
            ABI_A,
            7,
        )
        .unwrap();
        assert_eq!(
            er.commit_pending(&reg, ids[0]),
            Err(EndpointError::IdExhausted)
        );
        assert_eq!(er.endpoint_count(), 0, "id 容量不足 → 整批不落盘");
        assert_eq!(
            er.discover(&reg, ids[0], b"blk0", CONTRACT),
            Err(EndpointError::EndpointNotFound)
        );
    }

    // -- 4. lookup 校验 ------------------------------------------------------

    #[test]
    fn lookup_rejects_contract_and_abi_mismatch() {
        let (reg, ids) = ready_world();
        let mut er = EndpointRegistry::new();
        let endpoint = publish_ready(&mut er, &reg, ids[0], b"blk0", CONTRACT, 7);

        assert_eq!(
            er.lookup(&reg, endpoint, OTHER_CONTRACT, ABI_A),
            Err(EndpointError::ContractMismatch)
        );
        assert_eq!(
            er.lookup(&reg, endpoint, CONTRACT, ABI_B),
            Err(EndpointError::AbiMismatch)
        );
        assert_eq!(
            er.lookup(&reg, EndpointId::from_raw(999), CONTRACT, ABI_A),
            Err(EndpointError::EndpointNotFound)
        );
    }

    #[test]
    fn lookup_rejects_owner_that_is_not_ready_or_gone() {
        let (mut reg, ids) = ready_world();
        let mut er = EndpointRegistry::new();
        let endpoint = publish_ready(&mut er, &reg, ids[0], b"blk0", CONTRACT, 7);

        // owner 仍在注册表但离开 Ready（停止 / 失败）→ EndpointDead（endpoint 不再服务）。
        reg.begin_stop(ids[0]).unwrap();
        assert_eq!(
            er.lookup(&reg, endpoint, CONTRACT, ABI_A),
            Err(EndpointError::EndpointDead)
        );
        reg.finish_stop(ids[0]).unwrap();
        assert_eq!(
            er.lookup(&reg, endpoint, CONTRACT, ABI_A),
            Err(EndpointError::EndpointDead)
        );
        reg.mark_failed(ids[0]).unwrap();
        assert_eq!(
            er.lookup(&reg, endpoint, CONTRACT, ABI_A),
            Err(EndpointError::EndpointDead)
        );

        // owner 记录整体消失（身份不在）→ ProviderNotFound。
        let empty = Registry::new();
        assert_eq!(
            er.lookup(&empty, endpoint, CONTRACT, ABI_A),
            Err(EndpointError::ProviderNotFound)
        );
    }

    // -- 5. invalidate：永久失效 ---------------------------------------------

    #[test]
    fn invalidate_endpoint_is_permanent() {
        let (reg, ids) = ready_world();
        let mut er = EndpointRegistry::new();
        let endpoint = publish_ready(&mut er, &reg, ids[0], b"blk0", CONTRACT, 7);

        er.invalidate_endpoint(endpoint);
        assert_eq!(
            er.lookup(&reg, endpoint, CONTRACT, ABI_A),
            Err(EndpointError::EndpointDead)
        );
        // discover 也不交付死 endpoint。
        assert_eq!(
            er.discover(&reg, ids[0], b"blk0", CONTRACT),
            Err(EndpointError::EndpointDead)
        );
        // 幂等、不复活；记录保留（tombstone，id 不复用）。
        er.invalidate_endpoint(endpoint);
        assert_eq!(
            er.lookup(&reg, endpoint, CONTRACT, ABI_A),
            Err(EndpointError::EndpointDead)
        );
        assert_eq!(er.endpoint_count(), 1);
        assert_eq!(er.live_count(), 0);
        // 未知 id：静默 no-op。
        er.invalidate_endpoint(EndpointId::from_raw(999));
    }

    #[test]
    fn invalidate_provider_only_affects_its_own_endpoints() {
        let (reg, ids) = ready_world();
        let mut er = EndpointRegistry::new();
        let a1 = publish_ready(&mut er, &reg, ids[0], b"blk0", CONTRACT, 7);
        let a2 = publish_ready(&mut er, &reg, ids[0], b"blk1", CONTRACT, 8);
        let b1 = publish_ready(&mut er, &reg, ids[1], b"blk0", CONTRACT, 7);

        er.invalidate_provider(ids[0]);
        assert_eq!(
            er.lookup(&reg, a1, CONTRACT, ABI_A),
            Err(EndpointError::EndpointDead)
        );
        assert_eq!(
            er.lookup(&reg, a2, CONTRACT, ABI_A),
            Err(EndpointError::EndpointDead)
        );
        assert_eq!(
            er.lookup(&reg, b1, CONTRACT, ABI_A).unwrap().state,
            EndpointState::Live
        );
        assert_eq!(er.live_count(), 1);
    }

    // -- 6. EndpointId 单调、绝不复用 / 重定向 --------------------------------

    #[test]
    fn endpoint_ids_are_monotonic_and_never_reused() {
        let (reg, ids) = ready_world();
        let mut er = EndpointRegistry::new();
        let first = publish_ready(&mut er, &reg, ids[0], b"blk0", CONTRACT, 1);
        assert_eq!(first.raw(), 1, "EndpointId 从 1 起");

        // provider 失效 → 新实例发布同契约：id 严格更大，旧 id 绝不重定向。
        er.invalidate_provider(ids[0]);
        let second = publish_ready(&mut er, &reg, ids[1], b"blk1", CONTRACT, 1);
        assert!(second.raw() > first.raw());
        assert_eq!(
            er.lookup(&reg, first, CONTRACT, ABI_A),
            Err(EndpointError::EndpointDead),
            "失效的旧 endpoint 永不解析到新实例"
        );

        let third = publish_ready(&mut er, &reg, ids[2], b"blk2", CONTRACT, 1);
        assert!(third.raw() > second.raw());
    }

    // -- 7. discover：按 (provider, 名字, contract) 解析 -----------------------

    #[test]
    fn discover_resolves_per_provider_name_and_contract() {
        let (reg, ids) = ready_world();
        let mut er = EndpointRegistry::new();
        let a = publish_ready(&mut er, &reg, ids[0], b"blk0", CONTRACT, 7);
        let b = publish_ready(&mut er, &reg, ids[1], b"blk0", CONTRACT, 7);

        // 同名不同实例 → 各自 endpoint。
        assert_eq!(er.discover(&reg, ids[0], b"blk0", CONTRACT), Ok(a));
        assert_eq!(er.discover(&reg, ids[1], b"blk0", CONTRACT), Ok(b));

        // 未知名字 / 未知 provider / 契约不符。
        assert_eq!(
            er.discover(&reg, ids[0], b"missing", CONTRACT),
            Err(EndpointError::EndpointNotFound)
        );
        assert_eq!(
            er.discover(&reg, ComponentId::from_raw(99), b"blk0", CONTRACT),
            Err(EndpointError::EndpointNotFound)
        );
        assert_eq!(
            er.discover(&reg, ids[0], b"blk0", OTHER_CONTRACT),
            Err(EndpointError::ContractMismatch)
        );
    }

    #[test]
    fn duplicate_port_name_within_provider_is_rejected_atomically() {
        let (reg, ids) = ready_world();
        let mut er = EndpointRegistry::new();

        // 批内重复端口名 → 整批丢弃。
        er.stage_publish(
            &reg,
            ids[0],
            b"blk0",
            CONTRACT,
            InterfaceKind::Device,
            ABI_A,
            7,
        )
        .unwrap();
        er.stage_publish(
            &reg,
            ids[0],
            b"blk0",
            CONTRACT,
            InterfaceKind::Device,
            ABI_A,
            8,
        )
        .unwrap();
        assert_eq!(
            er.commit_pending(&reg, ids[0]),
            Err(EndpointError::DuplicatePort)
        );
        assert_eq!(er.endpoint_count(), 0);

        // 与已登记名字冲突（含 Invalid tombstone）→ 拒绝，不重定向到新 endpoint。
        let first = publish_ready(&mut er, &reg, ids[0], b"blk1", CONTRACT, 7);
        er.invalidate_provider(ids[0]);
        er.stage_publish(
            &reg,
            ids[0],
            b"blk1",
            CONTRACT,
            InterfaceKind::Device,
            ABI_A,
            9,
        )
        .unwrap();
        assert_eq!(
            er.commit_pending(&reg, ids[0]),
            Err(EndpointError::DuplicatePort)
        );
        assert_eq!(er.endpoint_count(), 1);
        assert_eq!(
            er.lookup(&reg, first, CONTRACT, ABI_A),
            Err(EndpointError::EndpointDead)
        );
    }

    // -- 8. in-flight call 记账（Registry） ------------------------------------

    #[test]
    fn call_accounting_increments_and_decrements() {
        let (mut reg, ids) = ready_world();
        assert_eq!(reg.active_calls(ids[0]), 0);

        reg.begin_call(ids[0]).unwrap();
        reg.begin_call(ids[0]).unwrap();
        assert_eq!(reg.active_calls(ids[0]), 2);

        reg.finish_call(ids[0]);
        assert_eq!(reg.active_calls(ids[0]), 1);
        reg.finish_call(ids[0]);
        // 未配对 finish：不下溢。
        reg.finish_call(ids[0]);
        assert_eq!(reg.active_calls(ids[0]), 0);

        // 实例之间互不影响；未知 id 的 finish 是 no-op。
        assert_eq!(reg.active_calls(ids[1]), 0);
        reg.finish_call(ComponentId::from_raw(99));
        assert_eq!(reg.active_calls(ComponentId::from_raw(99)), 0);
    }

    #[test]
    fn begin_call_rejects_non_ready_instances() {
        let (mut reg, ids) = ready_world();

        // Declared / Resolved / Starting 都不可服务：只有 Ready 放行。
        let declared = reg.declare(IMAGE).unwrap();
        assert_eq!(reg.begin_call(declared), Err(RegistryError::NotReady));
        reg.resolve(declared).unwrap();
        assert_eq!(reg.begin_call(declared), Err(RegistryError::NotReady));
        reg.begin_start(declared).unwrap();
        assert_eq!(reg.begin_call(declared), Err(RegistryError::NotReady));

        // Failed 后拒绝新调用；已在飞的调用仍可归还（finish 不设门禁）。
        reg.begin_call(ids[1]).unwrap();
        reg.mark_failed(ids[1]).unwrap();
        assert_eq!(reg.begin_call(ids[1]), Err(RegistryError::NotReady));
        assert_eq!(reg.active_calls(ids[1]), 1, "在飞计数保留");
        reg.finish_call(ids[1]);
        assert_eq!(reg.active_calls(ids[1]), 0);

        // 未知实例。
        assert_eq!(
            reg.begin_call(ComponentId::from_raw(99)),
            Err(RegistryError::NotFound)
        );
        assert_eq!(reg.active_calls(ComponentId::from_raw(99)), 0);
    }
}
