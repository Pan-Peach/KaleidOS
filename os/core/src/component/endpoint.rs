//! Component Endpoint Registry —— Contract / Endpoint 模型（**唯一绑定真相**）。
//!
//! 组件→组件依赖只走这里：数据模型 + 导出面（`kcore_endpoint_*`，call 实现见
//! `component/call.rs`）+ 生命周期接线（create 提交 / failure·stop 失效）。
//! `bind` 是 **Direct / Gate 的唯一选择点**。
//!
//! ```text
//! Contract：契约身份（kind + exact ABI fingerprint + 诊断名）—— 语义
//! Endpoint：某个组件实例在某个端口名上的一次发布 —— 存在
//! ```
//!
//! # 规则
//!
//! - **Contract ≠ Endpoint**：多个 provider 可以实现同一契约。契约由**首次发布**
//!   建立 `kind` / `abi`（[`ContractRecord`]），后续发布 kind / abi 不一致必须
//!   拒绝。
//! - **Endpoint 归属唯一**：`ComponentId` 就是实例身份；endpoint 生命周期内
//!   owner 不可变。两个实例可以发布**同名端口 + 同一契约**，各持不同 endpoint。
//! - **publish 创建新 endpoint，绝不覆盖**：没有"同 ABI 覆盖原槽"。端口名在
//!   **provider 实例内唯一**：重复发布同一端口名（含已失效名字）拒绝，不重定向。
//! - **consumer 只持有 [`EndpointId`]**：opaque capability。provider 交付的
//!   `api` / `ctx`（Direct 的 function table + state）与 `port`（Gate 的 dispatch
//!   token）存在 endpoint 记录上，但 Core **只存、永不解引用**；如何使用由
//!   [`EndpointRegistry::bind`] 在 bind 时按执行域选定。
//! - **EndpointId 单调、从 1 起、绝不回收 / 重定向**：provider 停止或失败后旧
//!   endpoint 永久 `Invalid`，绝不解析到新实例（`Invalid` 记录是 tombstone，
//!   id 不复用）。
//! - 标识符不带版本后缀：契约演进 = 原地替换（`AGENTS.md`）。
//!
//! # bind：Core 在绑定时刻选定调用机制（`docs/architecture/deployment.md` §2/§3）
//!
//! [`EndpointRegistry::bind`] 是 **Direct / Gate 的唯一选择点**，一次选定，运行期
//! 不再按调用重决策：
//!
//! ```text
//! 校验（exact contract + abi + 存活，复用 lookup）
//!   → 解析 (caller 执行域, provider 执行域)
//!   → select_mechanism：
//!       同域 KernelNative          → Direct（api/ctx 原样交付）
//!       KernelNative ↔ Isolated    → Gate（opaque EndpointId）
//!       Isolated ↔ Isolated        → Gate（同 AS 无法证明，绝不假设 Direct）
//!       Sandbox 参与              → 显式拒绝（ENOTSUP；绝不静默降级）
//! ```
//!
//! SDK / 组件**只执行**机制、**不得选择**机制：`api` / `ctx` 只在 Direct 结果里
//! 交付，Gate 结果不携带裸 function table（binding 不是可搬运的 POD）。
//!
//! # 表结构（真相 / 发现分离）
//!
//! - `endpoints`：endpoint 真相（[`EndpointRecord`] 是 `Copy`：无 `Vec`、无借用，
//!   `lookup` 直接返回值）。
//! - `contracts`：契约身份（首次发布建立，之后只校验）。
//! - `names`：发现表 `(provider, 端口名) → endpoint`（[`EndpointName`]）。单独
//!   成表，`lookup` 不被名字检索拖慢；只有 `discover` 查它。
//! - `pending`：staged publish 暂存。
//!
//! # Staged publish
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
//! 成功的 bind 发射 `TraceEvent::EndpointBind`（endpoint / provider / 选定的机制）。

use alloc::vec::Vec;
use spin::{Mutex, Once};

use crate::component::abi::InterfaceAbi;
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
/// `Pending` 预留给"已预留 id、尚未提交"的路径；当前
/// [`EndpointRegistry::stage_publish`] 不创建 endpoint，
/// [`EndpointRegistry::commit_pending`] 直接产出 `Live`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointState {
    Pending,
    Live,
    Invalid,
}

/// 一条 endpoint 真相（`Copy`：无 `Vec`、无借用）。
///
/// `api` / `ctx` 是 provider 发布的 **Direct** transport（function table + opaque
/// state），`port` 是 **Gate** transport（image 的 `kcomp_service_dispatch` 用它选
/// 契约）。Core 只存这些值、**永不解引用**，并只在 [`EndpointRegistry::bind`]
/// 选 Direct 时把 `api` / `ctx` 原样交给调用方。
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
    /// provider 的 `#[repr(C)]` function table 指针（Direct；Core 只存）。
    pub api: *const (),
    /// provider 的 opaque state（Direct；Core 原样回传）。
    pub ctx: *mut (),
}

// `api` / `ctx` 是 opaque provider 指针：Registry 只存取、永不解引用。Send/Sync
// 安全（指针本身只是字节；跨线程使用由外层 Mutex 串行化）。
unsafe impl Send for EndpointRecord {}
unsafe impl Sync for EndpointRecord {}

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
    api: *const (),
    ctx: *mut (),
}

// 同 [`EndpointRecord`]：opaque provider 指针，Registry 只存取。
unsafe impl Send for PendingPublication {}
unsafe impl Sync for PendingPublication {}

/// Endpoint 模型的拒绝原因（各子系统错误类型保持各自为政，不共用错误类型）。
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

/// 一个组件实例的执行域（`docs/architecture/deployment.md` §3 的模式矩阵）。
///
/// **今天只有 [`ExecutionDomain::KernelNative`] 真实存在**：Core 还没有部署 / 域
/// 字段（deployment.md §7.3），所有实例都跑在共享内核地址空间里。其余变体是矩阵
/// 的另一半——`select_mechanism` 的交叉臂已经在跑（host 测试覆盖），但**没有**
/// 任何“假装已实现”的路径：需要未实现机制的组合一律显式拒绝。
///
/// **本枚举只回答“在哪里、以什么特权 / 地址空间执行”**（placement）。**执行模型 /
/// ISA / runtime**（native machine code vs Wasm）是**正交维度**，不属于这里：
/// `KernelNative` / `IsolatedNative` / `SandboxedNative` 都可以承载 Wasm runtime，
/// `SandboxedNative` 也都可以是 native code——把 Wasm 塞进本枚举是把苹果和橘子
/// 放一起。Wasm 作为 Component 执行后端之一（`AGENTS.md` / `deployment.md` §1）
/// 需要**单独的维度**表达，不要加回本枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionDomain {
    /// 与 Core 同特权、同地址空间（今天唯一存在的域）。
    KernelNative,
    /// 同特权、私有地址空间（**未实现**：无私有 AS / `satp` 切换）。
    IsolatedNative,
    /// 低特权 + 私有地址空间（**未实现**：无 U-mode）。
    SandboxedNative,
}

/// Core 在 **bind 时**为一次调用选定的机制（一次，运行期不再重决策）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mechanism {
    /// 同域 KernelNative：provider 的 function table 直接调用（稳态零 Core 介入）。
    Direct,
    /// 跨域 / 需 containment：调用走 `kcore_endpoint_call` 的 Core call gate。
    Gate,
}

/// bind 的拒绝原因（`kcore_endpoint_bind` 的 ABI 翻译在 `errno.rs`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindError {
    /// endpoint 校验失败（未发布 / 已死 / owner 消失 / contract·abi 不符）。
    Endpoint(EndpointError),
    /// `(caller domain, provider domain)` 没有**已实现**的合法机制
    /// （跨特权 / 同 AS 无法证明且 syscall-IPC 未实现）→ `ENOTSUP`。
    /// **绝不静默降级成 Direct**（deployment.md §2 ⑤）。
    UnsupportedMechanism,
    /// 选中 Direct，但 provider 发布时没有交付 function table（`api` 为空）——
    /// 该 provider 无法服务 Direct → `ENOTSUP`。
    DirectWithoutApi,
}

impl From<EndpointError> for BindError {
    fn from(error: EndpointError) -> Self {
        Self::Endpoint(error)
    }
}

/// 一次成功 bind 的结果：endpoint 真相（`Copy`）+ Core 选定的机制。
///
/// `api` / `ctx` 只在 `mechanism == Direct` 时有意义；Gate 结果不携带裸 function
/// table（binding 不是可搬运的 POD）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundEndpoint {
    pub record: EndpointRecord,
    pub mechanism: Mechanism,
}

/// **机制选择的唯一决策函数**（Core owns truth；SDK / 组件只执行，不选择）。
///
/// 输入是**两端**的执行域（`docs/architecture/deployment.md` §3 矩阵），输出是
/// 该组合下**已实现**的合法机制；没有已实现机制的组合一律 `Err`——绝不静默
/// 降级成 Direct：
///
/// ```text
///                KernelNative   IsolatedNative   SandboxedNative
/// KernelNative   Direct         Gate             Gate
/// Isolated       Gate           Gate (*)         Gate
/// Sandboxed      reject         reject           reject
/// ```
///
/// (*) `Isolated ↔ Isolated`：矩阵允许“同一 AS 时 Direct”，但今天**无法证明**
/// 两个实例共享同一 AS（私有 AS 尚未实现），因此选 Gate——"同 AS 未知"绝不假设
/// Direct。Sandbox 参与的组合需要 syscall-IPC（未实现）或可证明的同 AS，一律拒绝。
///
/// **执行模型 / runtime（native machine code vs Wasm）不在本矩阵**——它与执行域
/// 正交（见 [`ExecutionDomain`] 文档），Wasm 需要单独的维度，不是第四个域。
pub fn select_mechanism(
    caller: ExecutionDomain,
    provider: ExecutionDomain,
) -> Result<Mechanism, BindError> {
    use ExecutionDomain::{IsolatedNative, KernelNative, SandboxedNative};
    match (caller, provider) {
        // 同域 KernelNative：单一内核 AS + 同特权 → Direct。
        (KernelNative, KernelNative) => Ok(Mechanism::Direct),
        // K ↔ I / K → S / I → S：同特权（或 Core 经 sret 进入 U）→ Gate。
        (KernelNative, IsolatedNative)
        | (IsolatedNative, KernelNative)
        | (KernelNative, SandboxedNative)
        | (IsolatedNative, SandboxedNative) => Ok(Mechanism::Gate),
        // I ↔ I：同特权但同 AS **无法证明** → Gate（绝不假设 Direct）。
        (IsolatedNative, IsolatedNative) => Ok(Mechanism::Gate),
        // Sandbox caller：需要 syscall-IPC（未实现）；同 AS 同样无法证明。
        // Sandbox **作为 callee** 的组合（K→S / I→S）已在上面判为 Gate。
        (SandboxedNative, _) => Err(BindError::UnsupportedMechanism),
    }
}

/// 解析一个实例的执行域（**唯一解析点**，deployment.md §1/§7.3）。
///
/// 从 registry 的组件记录读取部署域（`ComponentRecord::execution_domain`）——该字段
/// 由创建入口（`component/load.rs::create_component`）验证部署请求后写入，是 Core
/// owns 的部署真相。bind 对 **caller 与 provider 两端各解析一次**：合法机制同时
/// 取决于两端，绝不只按 provider 的部署标签决策。
///
/// 取已借用的 `&Registry`（而非自行取锁）：`bind` 在**持有 registry 锁**时解析
/// provider 域，再取一次锁会自死锁（`spin::Mutex` 不可重入）。未知 owner 回退
/// `KernelNative` 只是防御——`bind` 的存活校验已保证 owner 存在且 `Ready`。
pub fn instance_domain(components: &Registry, owner: ComponentId) -> ExecutionDomain {
    components
        .get(owner)
        .map_or(ExecutionDomain::KernelNative, |record| {
            record.execution_domain
        })
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
        api: *const (),
        ctx: *mut (),
    ) -> Result<(), EndpointError> {
        let record = components
            .get(provider)
            .ok_or(EndpointError::ProviderNotFound)?;
        // `Starting` = 正在 call_create；`Ready` 允许后续重发布。
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
            api,
            ctx,
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
                    api: p.api,
                    ctx: p.ctx,
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
                api: p.api,
                ctx: p.ctx,
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

    /// **纯存活解析**：endpoint 存在 + `Live` + owner 存在且 `Ready`。
    ///
    /// `kcore_endpoint_call` 的调用路径：call ABI 不携带 contract / abi，因为
    /// [`EndpointId`] 是 consumer 经 [`Self::lookup`] / [`Self::discover`] 拿到的
    /// **opaque capability**。发现路径只校验 contract（不携带 abi）；abi 由
    /// consumer 用 [`Self::lookup`]（导出面 `kcore_endpoint_validate`）自行核对。
    /// 这里只回答"现在还能不能调用"：死 endpoint、死 owner 一律拒绝
    /// （绝不把调用派发到已失效的实例）。
    pub fn resolve(
        &self,
        components: &Registry,
        id: EndpointId,
    ) -> Result<EndpointRecord, EndpointError> {
        let record = *self
            .endpoints
            .iter()
            .find(|r| r.id == id)
            .ok_or(EndpointError::EndpointNotFound)?;
        if record.state != EndpointState::Live {
            return Err(EndpointError::EndpointDead);
        }
        check_owner_live(components, record.owner)?;
        Ok(record)
    }

    /// consumer 按 `EndpointId` 解析：存活校验（[`Self::resolve`]）+ contract / abi
    /// 精确匹配，返回 `Copy` 记录（Core 验证后才交付；绝不交付死 endpoint）。
    pub fn lookup(
        &self,
        components: &Registry,
        id: EndpointId,
        contract: ContractId,
        abi: InterfaceAbi,
    ) -> Result<EndpointRecord, EndpointError> {
        let record = self.resolve(components, id)?;
        if record.contract != contract {
            return Err(EndpointError::ContractMismatch);
        }
        if record.abi != abi {
            return Err(EndpointError::AbiMismatch);
        }
        Ok(record)
    }

    /// **bind：Core 在绑定时刻选定调用机制**（`kcore_endpoint_bind` 的实现）。
    ///
    /// 先做与 [`Self::lookup`] 同一套校验（exact contract + abi + 存活），再按
    /// `(caller 执行域, provider 执行域)` 调 [`select_mechanism`]。Direct 结果要求
    /// provider 交付了 function table（`api` 非空）；Gate 结果不携带裸 function
    /// table。任何无已实现机制的组合都返回 [`BindError::UnsupportedMechanism`]。
    pub fn bind(
        &self,
        components: &Registry,
        id: EndpointId,
        contract: ContractId,
        abi: InterfaceAbi,
        caller_domain: ExecutionDomain,
    ) -> Result<BoundEndpoint, BindError> {
        // (1) 校验：contract + abi exact-match + 存活（与 validate 同一入口）。
        let record = self.lookup(components, id, contract, abi)?;
        // (2) 机制选择：两端执行域缺一不可。
        let mechanism = select_mechanism(caller_domain, instance_domain(components, record.owner))?;
        // (3) Direct 必须真的有 function table 可交付（结构性检查，不解引用）。
        if mechanism == Mechanism::Direct && record.api.is_null() {
            return Err(BindError::DirectWithoutApi);
        }
        // Trace：一次成功的绑定解析（Core 在此选定机制；payload 见 TraceEvent 文档）。
        crate::trace::emit(crate::trace::TraceEvent::EndpointBind {
            endpoint: record.id,
            provider: record.owner,
            mechanism,
        });
        Ok(BoundEndpoint { record, mechanism })
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

// —— 全局（boot/core::init 初始化；导出面 / 生命周期接线使用全局，测试用 new()）——

static ENDPOINTS: Once<Mutex<EndpointRegistry>> = Once::new();

/// 初始化全局 endpoint 注册表（core::init 调用一次）。
pub fn init() {
    ENDPOINTS.call_once(|| Mutex::new(EndpointRegistry::new()));
}

/// 取全局 endpoint 注册表（init 后可用）。
pub fn get_endpoints() -> &'static Mutex<EndpointRegistry> {
    ENDPOINTS.get().expect("endpoint registry not initialized")
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
    use crate::component::registry::{Registry, RegistryError};
    use alloc::vec::Vec;

    /// 声明一个测试组件（伪造、无 backing 的 loaded image）。
    fn declare(reg: &mut Registry, domain: ExecutionDomain) -> ComponentId {
        reg.declare(
            b"endpoint-test",
            crate::component::registry::test_support::test_loaded(0, None),
            domain,
        )
        .unwrap()
    }

    const CONTRACT: ContractId = ContractId::from_raw(0xC0DE_0001);
    const OTHER_CONTRACT: ContractId = ContractId::from_raw(0xC0DE_0002);
    const ABI_A: InterfaceAbi = InterfaceAbi::from_raw(0xAAAA_0001);
    const ABI_B: InterfaceAbi = InterfaceAbi::from_raw(0xBBBB_0002);

    /// Direct function table 的替身地址（Core 只存、不解引用）。
    static TABLE: [u8; 8] = [0; 8];

    /// 构造注册表并声明三个 Ready 实例（provider_a / provider_b / provider_c）。
    fn ready_world() -> (Registry, Vec<ComponentId>) {
        let mut reg = Registry::new();
        let mut ids = Vec::new();
        for _ in 0..3 {
            let id = declare(&mut reg, ExecutionDomain::KernelNative);
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
            core::ptr::null(),
            core::ptr::null_mut(),
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
            core::ptr::null(),
            core::ptr::null_mut(),
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
                core::ptr::null(),
                core::ptr::null_mut(),
            ),
            Err(EndpointError::ProviderNotFound)
        );

        // Declared / Resolved 拒绝；Starting / Ready 接受（create 期发布 = Starting）。
        let mut state = Registry::new();
        let declared = declare(&mut state, ExecutionDomain::KernelNative);
        assert_eq!(
            er.stage_publish(
                &state,
                declared,
                b"blk0",
                CONTRACT,
                InterfaceKind::Device,
                ABI_A,
                0,
                core::ptr::null(),
                core::ptr::null_mut(),
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
                core::ptr::null(),
                core::ptr::null_mut(),
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
            core::ptr::null(),
            core::ptr::null_mut(),
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
            core::ptr::null(),
            core::ptr::null_mut(),
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
            core::ptr::null(),
            core::ptr::null_mut(),
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
            core::ptr::null(),
            core::ptr::null_mut(),
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
            core::ptr::null(),
            core::ptr::null_mut(),
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
            core::ptr::null(),
            core::ptr::null_mut(),
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
            core::ptr::null(),
            core::ptr::null_mut(),
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
            core::ptr::null(),
            core::ptr::null_mut(),
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
            core::ptr::null(),
            core::ptr::null_mut(),
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

    // -- 4b. bind：Core 在绑定时刻选定调用机制 -------------------------------

    /// 发布一个带 Direct function table 的端口并提交（provider 已 Ready）。
    fn publish_ready_with_table(
        er: &mut EndpointRegistry,
        reg: &Registry,
        provider: ComponentId,
        port_name: &[u8],
        port: u32,
        api: *const (),
        ctx: *mut (),
    ) -> EndpointId {
        er.stage_publish(
            reg,
            provider,
            port_name,
            CONTRACT,
            InterfaceKind::Device,
            ABI_A,
            port,
            api,
            ctx,
        )
        .unwrap();
        er.commit_pending(reg, provider).unwrap();
        er.discover(reg, provider, port_name, CONTRACT).unwrap()
    }

    /// `select_mechanism` 覆盖**全部 9 个 (caller, provider) 组合**：
    /// 同域 KernelNative → Direct；同特权跨域（K↔I、I↔I、K→S、I→S）→ Gate；
    /// Sandbox 参与 → 显式拒绝（**绝不静默降级成 Direct**）。
    ///
    /// 执行模型 / runtime（native vs Wasm）不在矩阵内——它与执行域正交。
    #[test]
    fn select_mechanism_covers_the_domain_matrix() {
        use ExecutionDomain::{IsolatedNative as I, KernelNative as K, SandboxedNative as S};

        let cases: [(
            ExecutionDomain,
            ExecutionDomain,
            Result<Mechanism, BindError>,
        ); 9] = [
            (K, K, Ok(Mechanism::Direct)),
            (K, I, Ok(Mechanism::Gate)),
            (K, S, Ok(Mechanism::Gate)),
            (I, K, Ok(Mechanism::Gate)),
            // I ↔ I：同特权但同 AS **无法证明** → Gate（绝不假设 Direct）。
            (I, I, Ok(Mechanism::Gate)),
            (I, S, Ok(Mechanism::Gate)),
            // Sandbox caller 需要 syscall-IPC（未实现）→ 拒绝。
            (S, K, Err(BindError::UnsupportedMechanism)),
            (S, I, Err(BindError::UnsupportedMechanism)),
            (S, S, Err(BindError::UnsupportedMechanism)),
        ];
        for (caller, provider, expected) in cases {
            assert_eq!(
                select_mechanism(caller, provider),
                expected,
                "{caller:?} -> {provider:?}"
            );
        }
    }

    /// bind（同域 KernelNative）：机制 = Direct，`api` / `ctx` 从 endpoint 记录
    /// **原样**交付（Core 不解引用、不复制内容）。
    #[test]
    fn bind_same_domain_returns_direct_with_provider_table() {
        let (reg, ids) = ready_world();
        let mut er = EndpointRegistry::new();
        let mut state = 0u8;
        let ctx = &mut state as *mut u8 as *mut ();
        let api = &TABLE as *const u8 as *const ();
        let endpoint = publish_ready_with_table(&mut er, &reg, ids[0], b"blk0", 7, api, ctx);

        let bound = er
            .bind(
                &reg,
                endpoint,
                CONTRACT,
                ABI_A,
                ExecutionDomain::KernelNative,
            )
            .unwrap();
        assert_eq!(bound.mechanism, Mechanism::Direct);
        assert_eq!(bound.record.api, api);
        assert_eq!(bound.record.ctx, ctx);
        assert_eq!(bound.record.owner, ids[0]);
    }

    /// bind 成功发射 `TraceEvent::EndpointBind`（endpoint / provider / Core 在
    /// bind 时选定的机制）——该事件是机制决定的**唯一可观测点**。
    ///
    /// 用子序列断言（`assert_subsequence` 的文档：ring 是进程全局的，并行测试
    /// 会插入无关事件）；provider 用 `ids[2]` 让期望 payload 与本模块其它 bind
    /// 用例区分开。
    #[test]
    #[cfg(feature = "trace")]
    fn bind_emits_endpoint_bind_trace_with_selected_mechanism() {
        use crate::trace::{TraceEvent, test_support};

        let _trace = test_support::GUARD.lock();
        crate::trace::reset_for_test();

        let (reg, ids) = ready_world();
        let mut er = EndpointRegistry::new();
        let endpoint = publish_ready_with_table(
            &mut er,
            &reg,
            ids[2],
            b"blk2",
            9,
            &TABLE as *const u8 as *const (),
            core::ptr::null_mut(),
        );

        er.bind(
            &reg,
            endpoint,
            CONTRACT,
            ABI_A,
            ExecutionDomain::KernelNative,
        )
        .unwrap();

        test_support::assert_subsequence(
            &[TraceEvent::EndpointBind {
                endpoint,
                provider: ids[2],
                mechanism: Mechanism::Direct,
            }],
            &test_support::events(),
        );
    }

    /// bind 的校验与 validate 同源：contract / abi **exact-match** + 存活。
    #[test]
    fn bind_rejects_contract_abi_and_liveness_failures() {
        let (reg, ids) = ready_world();
        let mut er = EndpointRegistry::new();
        let endpoint = publish_ready_with_table(
            &mut er,
            &reg,
            ids[0],
            b"blk0",
            7,
            core::ptr::null(),
            core::ptr::null_mut(),
        );

        assert_eq!(
            er.bind(
                &reg,
                endpoint,
                OTHER_CONTRACT,
                ABI_A,
                ExecutionDomain::KernelNative
            ),
            Err(BindError::Endpoint(EndpointError::ContractMismatch))
        );
        assert_eq!(
            er.bind(
                &reg,
                endpoint,
                CONTRACT,
                ABI_B,
                ExecutionDomain::KernelNative
            ),
            Err(BindError::Endpoint(EndpointError::AbiMismatch))
        );
        assert_eq!(
            er.bind(
                &reg,
                EndpointId::from_raw(999),
                CONTRACT,
                ABI_A,
                ExecutionDomain::KernelNative
            ),
            Err(BindError::Endpoint(EndpointError::EndpointNotFound))
        );

        // provider 停止 / 失败 → endpoint 永久死亡，bind 绝不交付。
        er.invalidate_endpoint(endpoint);
        assert_eq!(
            er.bind(
                &reg,
                endpoint,
                CONTRACT,
                ABI_A,
                ExecutionDomain::KernelNative
            ),
            Err(BindError::Endpoint(EndpointError::EndpointDead))
        );
    }

    /// Direct 需要 provider 真的交付了 function table；`api` 为空 → 拒绝
    /// （不把 null table 交给调用方）。
    #[test]
    fn bind_rejects_direct_without_function_table() {
        let (reg, ids) = ready_world();
        let mut er = EndpointRegistry::new();
        // publish_ready 用空 api 发布（Gate-only provider 形状）。
        let endpoint = publish_ready(&mut er, &reg, ids[0], b"blk0", CONTRACT, 7);
        assert_eq!(
            er.bind(
                &reg,
                endpoint,
                CONTRACT,
                ABI_A,
                ExecutionDomain::KernelNative
            ),
            Err(BindError::DirectWithoutApi)
        );
    }

    /// 交叉组合在 bind 上表现为 Gate（不携带裸 function table）——机制由两端
    /// 执行域决定，不由 provider 的部署标签单独决定。
    #[test]
    fn bind_cross_domain_selects_gate_without_function_table() {
        let (reg, ids) = ready_world();
        let mut er = EndpointRegistry::new();
        let endpoint = publish_ready_with_table(
            &mut er,
            &reg,
            ids[0],
            b"blk0",
            7,
            &TABLE as *const u8 as *const (),
            core::ptr::null_mut(),
        );
        let bound = er
            .bind(
                &reg,
                endpoint,
                CONTRACT,
                ABI_A,
                ExecutionDomain::IsolatedNative,
            )
            .unwrap();
        assert_eq!(bound.mechanism, Mechanism::Gate);
        assert_eq!(bound.record.port, 7, "Gate 经 port + dispatcher 分派");
    }

    /// 绑定方向：**KernelNative caller → Isolated provider** 在 bind
    /// 上选 Gate（binding 只携带 opaque `EndpointId` + `port`；provider 域内的裸
    /// 入口绝不交付给另一个域）。
    #[test]
    fn bind_kernel_native_caller_to_isolated_provider_selects_gate() {
        let mut reg = Registry::new();
        let provider = {
            let id = declare(&mut reg, ExecutionDomain::IsolatedNative);
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            reg.finish_start(id).unwrap();
            id
        };
        let mut er = EndpointRegistry::new();
        // Isolated provider 的发布面（组件→Core publish trampoline）未实现：
        // endpoint 由组合方在 Core 侧登记，`api` 为空（Gate 不需要它）。
        let endpoint = publish_ready(&mut er, &reg, provider, b"svc0", CONTRACT, 7);
        let bound = er
            .bind(
                &reg,
                endpoint,
                CONTRACT,
                ABI_A,
                ExecutionDomain::KernelNative,
            )
            .unwrap();
        assert_eq!(bound.mechanism, Mechanism::Gate);
        assert_eq!(bound.record.port, 7, "Gate 经 port + dispatcher 分派");
        assert!(
            bound.record.api.is_null(),
            "Gate 绝不交付 provider 域内的裸入口"
        );
    }

    /// `resolve` 是 call ABI 的纯存活解析：不携带 contract / abi（id 本身是
    /// 组合期经 lookup / discover 交付的 opaque capability），只回答"还能不能调用"。
    #[test]
    fn resolve_is_liveness_only() {
        let (mut reg, ids) = ready_world();
        let mut er = EndpointRegistry::new();
        let endpoint = publish_ready(&mut er, &reg, ids[0], b"blk0", CONTRACT, 7);

        // 活 endpoint + Ready owner：resolve 返回记录（无需再传 contract / abi）。
        let record = er.resolve(&reg, endpoint).unwrap();
        assert_eq!(record.owner, ids[0]);
        assert_eq!(record.port, 7);

        // 未知 id → EndpointNotFound。
        assert_eq!(
            er.resolve(&reg, EndpointId::from_raw(999)),
            Err(EndpointError::EndpointNotFound)
        );

        // endpoint 永久失效 → EndpointDead。
        er.invalidate_endpoint(endpoint);
        assert_eq!(er.resolve(&reg, endpoint), Err(EndpointError::EndpointDead));

        // owner 离开 Ready（停止 / 失败）→ EndpointDead；身份消失 → ProviderNotFound。
        let other = publish_ready(&mut er, &reg, ids[1], b"blk1", CONTRACT, 8);
        reg.begin_stop(ids[1]).unwrap();
        assert_eq!(er.resolve(&reg, other), Err(EndpointError::EndpointDead));
        let empty = Registry::new();
        assert_eq!(
            er.resolve(&empty, other),
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
            core::ptr::null(),
            core::ptr::null_mut(),
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
            core::ptr::null(),
            core::ptr::null_mut(),
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
            core::ptr::null(),
            core::ptr::null_mut(),
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
        let declared = declare(&mut reg, ExecutionDomain::KernelNative);
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
