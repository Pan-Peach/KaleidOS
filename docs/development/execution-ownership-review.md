# Execution & Ownership Semantics Review

本页记录第二轮执行语义与资源归属审计，以及后续修复；**不是架构契约**。
审计基线：`develop`，`69a3b436d830e8902dff428f361e0e0db05f8a0b`，2026-10-08。
§1–§11 保留原始审计基线；其中“当前”“下一轮”指审计时的状态。用户随后授权修复，
已实施的变更与新验证见 §12。生产源码证据链接固定到审计 commit，避免修复后行号漂移。

权威入口仍是 [文档索引](../README.md)：
[组件生命周期](../architecture/component-lifecycle.md)、
[部署](../architecture/deployment.md)、[驱动](../architecture/driver-model.md)、
[内存与堆](../architecture/memory-and-heap.md)、[调度](../architecture/scheduling.md)。
已修复的问题以 §12 为准；其余建议仍未实施，不能从建议推定功能已实现。

## 1. 判断与证据边界

当前 `ComponentId` 足以表示完整运行实例及其资源归属；`TaskId` 表示受调度的执行流。
一个实例可以发布被动入口、拥有 Worker，或同时拥有两者。Direct、Gate、Worker 可以共存，
无需把调用方式变成组件身份，也没有证据要求新增 OwnerId、ExecutionContextId 或 ProtectionDomainId。
依据：[实例记录](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/registry.rs#L54)、
[任务记录](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/task/record.rs#L12)、
[混合组件 fixture](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/components/tests/kcomp_checksum/src/lib.rs#L92)。

复杂性的主要来源是三件事：把“当前 principal”误读成“当前函数的 provider”；
以同一同步接口隐藏等待、栈与故障边界差异；生命周期验证和资源提交没有处处组成同一事务。
前两者需要明确语义，后一项需要小范围实现修补。另有 IRQ 临界区和嵌套生命周期调度的真实实现缺陷。
推荐**方案 B：以 A 的修补为前置，收敛少量语义规则，不新增身份注册表**。

本页使用四种证据层次：

| 标记 | 含义 | 本轮实际范围 |
|---|---|---|
| S | 源码事实 | 直接检查生产路径、ABI、SDK 与 fixture；链接行号固定于上述基线 |
| I | 源码交错/调用链推演 | 给出可达路径或 SMP 交错；证明代码允许错误，不表示已在硬件复现 |
| T | 实际执行的测试 | 本轮 Core host 测试，结果见 §11；host fake 不证明真实切栈、IRQ、TLB 或隔离 |
| D | 未实现的设计推演 | U-mode 组件服务、私有域设备 transport、未来安全回收等 |

`Confirmed Bug` 指代码已违反可明确陈述的不变量；也可以有 S/I 证据而尚无硬件复现。
`Semantic Inconsistency` 是当前能力/命名/契约未形成清楚规则；`Intentional Limitation` 是明确接受的限制；
`Future Gap` 是尚未实现的能力；`Documentation Mismatch` 是描述与现有实现不符。
§7 每个问题只给一个主分类，不把所有限制都称为 bug。

路径校正：任务中的 containment 位于 `os/core/src/component/containment.rs`；
AddressSpace 实现当前是 `os/core/src/memory/address_space.rs` 单文件。

## 2. 三个独立概念与当前身份模型

### 2.1 归属、执行、保护

**Component Ownership** 回答对象的生命周期归谁：实例记录、Task owner、device claim、
IRQ route、DMA allocation、DMA mapping、endpoint、AS 各有自己的真相。
`ComponentId` 被这些记录引用，并不意味着每个对象都必须由同一种上下文创建。
通用 MemoryView 与 heap 对象**没有 Core 的逐对象 owner 账本**，不能给它们虚构 owner。
依据：[Memory API](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L130)、
[DMA 两类记录](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/dma.rs#L111)。

**Execution Context** 回答当前执行流、边界链、栈和 AS 是什么。
当前 Task owner 不随函数调用改变；当前 principal 可以因 Core 建立 Init、Exit、Gate、IRQ、Policy 边界而改变。
Core 不靠当前 PC 反查每一个普通函数的所属 image，也不在 Direct 调用时插入边界。
依据：[ambient](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/context.rs#L38)、
[边界种类](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/containment.rs#L164)、
[Direct 前端](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/components/kcomp-sdk/src/block/backend.rs#L95)。

**Protection Boundary** 回答访问如何被限制。
KernelNative 同特权、共享地址空间，owner 检查是可信代码的记账与拆除约束；裸 MMIO、ctx、函数表一经交付，
不经过 Core 的逐次访问检查。IsolatedNative 真正切换 AS，限制普通地址可见性，
但仍在 S-mode、共享 Core 映射，不能防御恶意特权代码。SandboxedNative 的组件装载/服务 transport 未实现；
现有 UserDomain 的 U-mode 用户程序执行不能据此变成已实现的 U-mode `.kcomp`。
依据：[claim](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/device.rs#L236)、
[Isolated 入口](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/isolated_lifecycle.rs#L448)、
[共享映射](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/memory/address_space.rs#L858)、
[Sandbox 拒绝](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/load.rs#L328)、
[UserDomain step](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/task/user.rs#L348)。

### 2.2 身份与唯一真相

| 身份/字段 | 当前来源及语义 | 是否等价于其他身份 |
|---|---|---|
| ComponentId | Registry 单调分配，完整实例的身份；失败/停止留记录；[registry:109](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/registry.rs#L109) | 同时可作为资源 owner 的值；不等价于 Task、AS 或安全凭证 |
| TaskId | TaskTable 创建执行记录；[table:54](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/task/table.rs#L54) | 标识执行流，不表示当前 provider |
| Task owner | 创建时传入 principal，TaskRecord 私有字段，无改 owner API；[record:12](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/task/record.rs#L12)、[task:121](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/task/mod.rs#L121) | 不因 Direct/Gate 改变；entry 的 image 范围检查只约束初始入口 |
| Current principal | `ambient()`：当前 active guard 若有 owner 则优先，否则 current Task owner → loader `creating` fallback；[context:38](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/context.rs#L38) | 是本次 Core 调用的归属来源，不保证正在执行该 owner 的任意函数 |
| RequestContext.task | Task 边界带本 Task；Service 带 caller Task provenance；Init/Exit/IRQ/Policy 无 task；[containment:257](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/containment.rs#L257) | provenance，不授予 provider 对 caller Task 的控制权；不总等于 scheduler current Task |
| Caller | Gate 入场时从 ambient 获取；Policy caller 是 Core；[call:248](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/call.rs#L248)、[containment:865](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/containment.rs#L865) | Direct 没有 Core 管理的 caller/provider 记录；业务仍可知道 binding 的来源 |
| Provider | EndpointRecord.owner 经 registry 查实例；Direct 的表和 ctx 来自该 endpoint；[endpoint:130](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/endpoint.rs#L130) | provider 与调用时 principal 可以不同 |
| Device owner | DeviceTable.slot.owner，在 claim 提交；DeviceId 是发现身份；[device:137](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/device.rs#L137) | 与 MMIO 当前访问者无强制对应关系 |
| DMA allocation owner | 分配时 ambient component；Allocation 保存 backing lease；[dma:265](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/dma.rs#L265) | 不要求与设备或 mapping owner 相同 |
| DMA mapping owner | `map` 从已 claim device 的 owner 得到，忽略 ctx.component；[dma:289](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/dma.rs#L289) | 是设备映射生命周期，不是 backing 的分配归属 |
| IRQ owner | register 要求 ambient == device owner，route 保存 owner；投递从 route 取 owner；[irq resource:313](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/irq.rs#L313)、[IRQ admission:121](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/irq/mod.rs#L121) | IRQ principal 是 route owner，不是被打断 Task owner；handler 地址不构成身份认证 |
| AS owner | KernelAddressSpace.owner 来自 Core create/adopt；[AS:174](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/memory/address_space.rs#L174)、[AS:598](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/memory/address_space.rs#L598) | AS 使用者与 owner 不必一致；共享 Core 映射不属于独占组件语义 |
| Endpoint owner | 发布时 Init principal，stage 后提交；[export:610](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L610)、[endpoint:350](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/endpoint.rs#L350) | 发布归属与实例生命周期，不授予任意资源权限 |
| Instance State | create 的 opaque out_state；Registry 只保存，dispatcher/destroy 取回；[load:234](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/load.rs#L234)、[registry:143](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/registry.rs#L143) | ctx 不必等于 state，不能从其中未经验证的 id 获得授权 |

需要区别“重复真相”和“不同事实引用同一个 ComponentId”：

- Registry.domain 与 EndpointRecord.owner 分别是部署和发布事实；不应合并。
- ComponentRecord.address_space 是 Isolated 部署的主 AS 引用；AS 表是映射/生命周期真相，
  UserDomain 另持自己的 AS 引用。不是多个表各自决定同一 owner。
- EndpointName 中 provider/name 的索引由 stage 建立，EndpointRecord.owner 仍是 provider 真相；
  改索引须维护一致性，但没有证据需要新增 directory。
- VirtIO HAL 的 `device_addr → mapping id` 缓存只是协议适配反查；Core DMA 表负责归属。
  当前 Mapping 没有保存 backing 区间或 pin，这是缺失关系，不能靠合并 owner 字段补足。
- UserDomain.mappings 与 AS 表的私有映射确实重复保存区间、权限和物理映射信息：
  前者用于用户缓冲复制/权限验证，后者负责映射提交。当前 map 先预留元数据容量、成功提交 AS 后记录本地副本；
  protect 先构建完整新 AS，再一起替换 space/mappings，公开 ABI 没有另一路直接修改这个 AS。
  本轮未发现这两份记录实际分歧，但需保持单一更新路径；未来若开放新的修改入口，应收敛查询或统一提交，
  不能把副本当作互不约束的第二真相，也不必为此新增 owner 类型。
- `creating` 与 Init guard 都能表达创建归属，是值得收窄的临时身份通道；当前 `creating`
  还用于构造 runtime-init/create guard，不能未审计调用点就删掉。查询 fallback 与正常 guard 身份并存，
  更不能用 fallback 修复调度后丢失的 Init 边界。

依据：[AS 引用登记](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/registry.rs#L152)、
[UserDomain](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/task/user.rs#L110)、
[endpoint 索引](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/endpoint.rs#L164)、
[HAL 缓存](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/components/drivers/virtio_blk/src/hal.rs#L30)、
[User mapping 提交](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/task/user.rs#L135)、
[User protect 交换](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/task/user.rs#L319)、
[creating 用途](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/containment.rs#L643)、
[loader 保存恢复](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/load.rs#L219)。

### 2.3 ComponentRecord 与执行域没有必要一一绑定

Loaded image、opaque state、生命周期、id 是当前完整实例的合理字段；execution_domain 是该实例的部署选择。
inflight 是 Core 可观察的 Gate/Policy/IRQ 活动计数，不是所有正在运行该 image 函数的计数。
address_space 单个 Option 是当前 Isolated 入口/栈部署的主空间限制，不是“一个组件只能拥有一个 AS”的公理。
依据：[record](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/registry.rs#L54)、
[call/IRQ admission](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/registry.rs#L257)。

一个 owner 可以已有多个 AS：每个 `UserDomain::new(owner)` 都建立新空间，
用户任务由 KernelNative personality 的 ComponentId 拥有；不会写入该组件的主 `address_space`。
ASManager 不检查 owner 的唯一性。一个 KernelNative boot AS 则容纳多个组件。
一个 Task 的初始 entry 被限制在 owner image 中，但后续可以 Direct 执行其他 image，或 Gate 执行 provider。
组件的方法并不必须在自己的 Worker 上执行。
依据：[UserDomain 创建](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/task/user.rs#L121)、
[AS create/adopt](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/memory/address_space.rs#L598)、
[task entry 检查](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/task/mod.rs#L121)、
[SDK Direct/Gate](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/components/kcomp-sdk/src/block/backend.rs#L95)。

因此无需为当前可表达的这些关系增加新 ID。未来两个私有组件共享一个受控 AS、
一个实例有多个服务执行空间等场景需要具体入口/地址选择规则；现有 Isolated 主 AS 字段尚不支持这种部署，
但这也不证明必须新增全局 ProtectionDomain Registry。

## 3. 公开 Core ABI：61 项逐项矩阵

范围以 [abi/core.toml](../../abi/core.toml) 的 61 个 `[[function]]` 为准，包含拆分到
`component/export/user.rs` 与 `export/query.rs` 的入口。表中 `P` 表示 `RequestContext::ambient().component`；
`Tcur` 表示 scheduler current Task；`K/I/S` 表示 KernelNative/IsolatedNative/SandboxedNative。
“跨机制一致”描述**当前能力和结果**，不是对未来 transport 的承诺。
表中“拒绝调度链”指祖先含 IRQ、ServiceCall、PolicyCall；“owner 匹配”是可信 Native 的记账约束。

[核心哲学 §6](../philosophy/core-philosophy.md#L308) 的显式 RequestContext 原则需要在
ABI 边界与内部机制之间理解：ABI 从 Core 已建立的上下文解析一次身份，内部机制接收明确的 requester/对象引用。
当前一些内部操作仍按职责读取 current Task，显式参数也并非全部采用同一结构。
这不能被解释为“公开 ABI 不得解析 ambient”，或为了形式统一给无 owner 查询强加 RequestContext；
必要的是避免内部偷偷把 current Task 当 provider，并保持最终提交处的真实复验。

### 3.1 Component 生命周期（3）

| Core API / 实现 | 调用时 Principal 来源 | 操作对象 Owner 来源 | 使用的执行上下文 | 隐含假设 | 是否跨机制一致 |
|---|---|---|---|---|---|
| `kcore_component_create` [E:558](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L558) | 不读取 P | 新 Registry 实例自有 id；没有 parent owner | 默认 K；建立新实例 Init；Policy 祖先拒绝 | 可信调用、配置字节借用；无 Failed caller 检查 | 否：Task/Direct/Gate/IRQ 可走加载；无完整上下文门禁，B6 |
| `kcore_component_load` [E:529](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L529) | 不读取 P | 同上 | 请求 K/I/S，S 返回 ENOTSUP；默认配置 | 名称/domain 校验；同样无 Failed caller 检查 | 同 create；I caller 的 import 不支持此 API |
| `kcore_component_stop` [E:481](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L481) | P，拒绝 Failed/非 K | 显式 ComponentId 定位目标，无 caller-target owner 匹配 | 拒绝调度链；检查 target Ready、live Tasks、Direct 表、inflight | 受信 K 可停止其他实例；destroy 用目标 Exit principal | 与 create 门禁不一致；成功逻辑/保活约束见 §5 |

### 3.2 Task 创建、运行与同步（9）

| Core API / 实现 | 调用时 Principal 来源 | 操作对象 Owner 来源 | 使用的执行上下文 | 隐含假设 | 是否跨机制一致 |
|---|---|---|---|---|---|
| `kcore_task_create` [E:902](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L902) | P，拒绝 Failed/非 K | 新 Task.owner=P | 拒绝调度链；registry 锁内复验 Starting/Ready、entry 在 P image | 不按函数地址选择 provider；arg 不授权 | Direct B entry 在 A 下通常 EntryOutOfImage；Gate/IRQ 拒绝 |
| `kcore_task_start` [E:929](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L929) | P | TaskRecord.owner，须等于 P | 拒绝调度链，home CPU 选当前 CPU；registry→Task 提交 | Starting/Ready，Created→Runnable | Direct 操作 A Task；Gate/IRQ 拒绝 |
| `kcore_task_start_on` [E:939](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L939) | P | 同 start | 同上，指定在线 CPU，提交后 IPI | CPU home 约束防并发进入未保存 Context | 与 start 同语义 |
| `kcore_cpu_current` [E:952](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L952) | N/A | N/A | 当前 CPU 编号 | 只读快照，不是 owner | 调用方式不改 CPU；I import 暂不支持 |
| `kcore_task_yield` [E:961](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L961) | 不用 P 授权 | Tcur 的 TaskRecord，owner 不变 | 拒绝调度链，然后 current Task 调度 | 必须有 current Task；未验证 Init/Exit 与 Tcur 一致 | Direct yield A；Gate/IRQ 拒绝；嵌套生命周期 B4 |
| `kcore_task_park` [E:966](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L966) | P，拒绝 Failed/非 K | 被阻塞对象=Tcur，不是 P 的新等待对象 | 拒绝调度链；permit 或原子 Blocked 提交 | 不把 Gate caller_task 当可 park 对象 | Direct park A；Gate/IRQ 拒绝；B4 同样适用 |
| `kcore_task_unpark` [E:982](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L982) | P | 目标 Task.owner=P；registry 锁内复验 | 非切换操作，Gate/IRQ 允许，Policy 拒绝 | 提前 permit 合并；跨 owner 拒绝 | Direct 只能唤醒 A Task；B Gate/IRQ 可唤醒 B Task |
| `kcore_task_exit` [E:1000](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L1000) | 不用 P 授权 | Tcur | 拒绝调度链；Exited 后不再返回 | Task 退出不等于 Component Stop | Direct 退出 A；Gate/IRQ 拒绝；生命周期 B4 |
| `kcore_task_state` [E:1006](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L1006) | N/A | 只读 TaskRecord.owner，不参与权限判定 | 任意 Native 查询 | 无 owner 隔离的全局观察 | 机制无身份切换影响；I import 暂不支持 |

### 3.3 Memory / AddressSpace / Heap（4）

没有通用的公开 AS create/map ABI；AS 的受控操作由 Isolated loader、backing 和 UserDomain 入口编排。

| Core API / 实现 | 调用时 Principal 来源 | 操作对象 Owner 来源 | 使用的执行上下文 | 隐含假设 | 是否跨机制一致 |
|---|---|---|---|---|---|
| `kcore_memory_acquire` [E:157](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L157) | P 选择部署/主 AS；无 P 取 K 路径；有 P 拒绝 Failed | N/A：没有 region owner；I 有精确 AS mapping | 本域访问窗口；不要求普通 Task | K 可信；I 动态 extent；不记 malloc 对象/字节 quota | K Direct/Gate 都共享 VA；I 返回本域 VA；S 未支持 |
| `kcore_memory_release` [E:228](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L228) | P 选择部署/AS；无 Failed 拒绝 | N/A；I exact mapping 限定空间 | 同本域释放，允许 teardown | 原样 view；K shape 校验后 raw free，调用方保证不再借用 | K 无 caller owner；I 不能释放其他 AS/image/栈；不透明跨域交付不可直接释放 |
| `kcore_heap_alloc` [E:272](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L272) | N/A | N/A：共享 heap 不记对象 owner | K 窄 ABI；函数体无 P/Failed 检查 | layout、共享分配器；I/S loader 拒绝符号 | K 调用方式无差别；I 私有 HeapState，不能调用这两个符号 |
| `kcore_heap_dealloc` [E:292](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L292) | N/A | N/A | 同上 | ptr/layout 精确匹配由可信调用方保证 | 同 alloc；不表示全域回收 |

### 3.4 Device（3）

| Core API / 实现 | 调用时 Principal 来源 | 操作对象 Owner 来源 | 使用的执行上下文 | 隐含假设 | 是否跨机制一致 |
|---|---|---|---|---|---|
| `kcore_device_nth` [E:1080](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L1080) | N/A | N/A：枚举返回 DeviceId 身份 | 只读 discovery | 发现不等于 claim/权限 | 机制不改变发现结果；I import 未支持 |
| `kcore_device_claim` [E:1125](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L1125) | P，拒绝 Failed/非 K | DeviceTable.owner←P | local irq-save + device 锁；尚无 lifecycle 同事务复验 | 设备独占且未 quarantine；交付 MMIO window | Direct 新 claim=A；B Init/Worker/Gate= B；B1 race |
| `kcore_device_release` [E:1162](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L1162) | P，无 Failed 拒绝 | DeviceTable.owner 必须=P | device 锁涵盖 IRQ/DMA 子资源存在检查与解除 claim | 无 live route/map 才 release；裸指针协作撤销 | Direct A 不能 release B；B teardown 可；已修 release/map 原子性 |

### 3.5 IRQ（4）

| Core API / 实现 | 调用时 Principal 来源 | 操作对象 Owner 来源 | 使用的执行上下文 | 隐含假设 | 是否跨机制一致 |
|---|---|---|---|---|---|
| `kcore_irq_register` [E:1189](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L1189) | P，拒绝 Failed/非 K | device owner 必须=P，route.owner=P | device→IRQ 插入，local irq-save | handler/ctx 可信且足够驻留；生命周期提交有 B1 race | Direct A 不能为 B device 注册；B Init/Worker/Gate 可 |
| `kcore_irq_enable` [E:1218](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L1218) | P，无 Failed 拒绝 | device owner=P + existing route | 表校验后锁外硬件 enable；缺 irq-save | 未与 release/re-register 串行提交 | owner 规则一致；临界区 B2、硬件提交 B3 |
| `kcore_irq_disable` [E:1232](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L1232) | P，无 Failed 拒绝 | 同 enable | 同上，硬件 disable | teardown 可用；仍有 B2/B3 | 同 enable |
| `kcore_irq_release` [E:1247](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L1247) | P，无 Failed 拒绝 | device/route owner=P | irq-save；表内移除、锁外 disable | 不等待已准入 callback 完成；与 Stop inflight 分开 | Direct A 不能拆 B；B3；返回不是 callback drain |

### 3.6 DMA（4）

| Core API / 实现 | 调用时 Principal 来源 | 操作对象 Owner 来源 | 使用的执行上下文 | 隐含假设 | 是否跨机制一致 |
|---|---|---|---|---|---|
| `kcore_dma_alloc` [E:1271](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L1271) | P，拒绝 Failed/非 K | Allocation.owner=P | device-agnostic，物理连续，local irq-save 后记表 | 新 allocation admission 有 B1 race | Direct B 内分配=A；K Gate=B；I/S import 不支持 |
| `kcore_dma_free` [E:1301](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L1301) | P | Allocation.owner 必须=P | 移出 allocation 表，backing 进 quarantine | 没有设备静默证明，不实际 free；不检查 mapping 依赖 | A 可拆 Direct 中 A 分配；B teardown 不能拆该 A allocation |
| `kcore_dma_map` [E:1320](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L1320) | P 用于 caller Failed/K 门禁；资源层忽略 ctx | Mapping.owner=device.owner | device→DMA 锁内复验 claim 并插入；无需 allocation 来历 | 受信共享 RAM；不 pin、不保存范围、不复验 device owner lifecycle | K Direct/Gate 可同为 B mapping；B1；I/S 不支持 |
| `kcore_dma_unmap` [E:1368](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L1368) | N/A，不解析 caller | mapping id 找记录，owner 仅记 trace | 删除逻辑 mapping | Native id 不是隔离域凭证；不停止 DMA | K 不随 principal 改变；不能原样作为未来 U 授权 |

### 3.7 Endpoint / Service Call（5）

| Core API / 实现 | 调用时 Principal 来源 | 操作对象 Owner 来源 | 使用的执行上下文 | 隐含假设 | 是否跨机制一致 |
|---|---|---|---|---|---|
| `kcore_endpoint_publish` [E:610](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L610) | `ambient_init()`，只认最内层 Init | staged provider=P | runtime-init/create 边界；registry→endpoint | 发布 opaque api/ctx，create 成功才提交；精确 ABI | Worker/Direct/Gate/IRQ 无发布权限；嵌套 create 是新 provider |
| `kcore_endpoint_lookup` [E:663](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L663) | N/A | 名称/contract 指向 record.owner | 只读发现，检查 provider Ready | 发现 id 不授予 private pointers | K/I 支持；无 call 计数 |
| `kcore_endpoint_validate` [E:705](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L705) | N/A | EndpointRecord.owner + lifecycle | 只读 exact contract/ABI 检查 | 返回后状态可变；不等于持有引用 | K/I 支持；Direct 每次调用不自动 validate |
| `kcore_endpoint_bind` [E:742](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L742) | P，先检查 Failed | provider 来自 endpoint；caller/provider domain 来自 Registry | registry→endpoint；K/K Direct，K/I/I Gate，含 S 拒绝 | Direct 表非空；不记 binding 数量；caller gate 有先查后锁窗口 | 机制明确不同；K/K Gate-only 不能经普通 bind 得到 Gate，S2 |
| `kcore_endpoint_call` [E:823](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L823) | caller=P；入场后 principal=provider | EndpointRecord.owner | provider Ready+inflight；caller Task 仅 provenance；独立 stack/AS；IRQ/Policy/reentry 拒绝 | flat frame；两层 transport/method status；不能 yield/park | K 原生显式 Gate 与 I Gate 同边界规则；I outbound marshal；不同故障/指针语义 |

### 3.8 Scheduling Policy（2）

| Core API / 实现 | 调用时 Principal 来源 | 操作对象 Owner 来源 | 使用的执行上下文 | 隐含假设 | 是否跨机制一致 |
|---|---|---|---|---|---|
| `kcore_sched_run` [E:1051](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L1051) | 不用 P 授权 | N/A：调度锚点；被选择 Task 有独立 owner | 无 current Task 才能 run；拒绝调度链 | anchor 的 Init 可运行自己创建的 Task；不据 provider 变更 Task owner | Direct worker 不能重复 run；Gate/IRQ/Policy 拒绝 |
| `kcore_sched_set_policy` [E:1062](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L1062) | 不读取 caller P；context gate | endpoint provider，须 K/Ready/exact scheduler contract | 拒绝调度链；预分配 policy stack；busy 防并发替换 | 受信全局 policy replacement；Core 复验提案，不授予调度提交权 | I provider 已被拒绝；不存在“对 I dispatcher 原生直跳”的本轮 bug |

### 3.9 UserDomain（11）

所有入口先由 [user::owner](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export/user.rs#L6) 取得 P，要求 K 且 Starting/Ready。
实际 U 执行仅 rv64 + S-mode + MMU 路径支持；其他配置返回 ENOTSUP。
这里的 Component owner 是 personality，U 代码属于用户任务的私有映射，并非新的组件实例。

| Core API / 实现 | 调用时 Principal 来源 | 操作对象 Owner 来源 | 使用的执行上下文 | 隐含假设 | 是否跨机制一致 |
|---|---|---|---|---|---|
| `kcore_user_create` [U:25](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export/user.rs#L25) | P | 新 Task.owner=P；新 UserDomain.space.owner=P | task_create 的 image entry/调度链门禁 | entry 是 native personality trampoline，不是直接 U PC | Direct 属 A；Gate/IRQ 创建拒绝；I 不支持 |
| `kcore_user_map` [U:40](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export/user.rs#L40) | P | 目标 Task.owner=P；AS 在该 Task.user | Created 或 current Task 可访问；区间/PTE/backing 提交 | 不是通用组件 AS 映射；无独立 region owner | Direct 操作 A user；Gate B 可操作 B Created user；上下文规则 S3 |
| `kcore_user_protect` [U:50](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export/user.rs#L50) | P | 同 map | 受控重建 AS，成功交换、退役旧空间 | 不改变 Component owner；失败保留原空间 | 同 map |
| `kcore_user_load` [U:75](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export/user.rs#L75) | P | Task.owner=P + task.user mapping | 仅 Created 装载；全范围写权限验证后复制 | native source 借用，U 目标按托管 mapping 校验 | 同 map；不能当 U component 任意写能力 |
| `kcore_user_read` [U:83](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export/user.rs#L83) | P | 同上 | Created 或本 current user Task；读权限全范围检查 | Core 复制到 native buffer，不直接信任 U 指针 | 与 owner 规则一致；Gate 无 caller Task 权限 |
| `kcore_user_write` [U:91](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export/user.rs#L91) | P | 同上 | Created 或本 current user Task；写权限全范围检查 | 同 read | 同 read |
| `kcore_user_prepare` [U:99](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export/user.rs#L99) | P | 目标 Task.owner=P | Created，校验 PC/SP，标记 prepared | 提交 U 寄存器起点，尚未执行 | Gate 可处理 B 的 Created Task；I 不支持 |
| `kcore_user_step` [U:108](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export/user.rs#L108) | P | Tcur.owner=P + UserDomain.space | 仅本 CPU Running user Task；调度链拒绝；切 AS/U，trap 回 personality | trap/timer 是真实 Arch 契约；不支持 Gate 借用 A Task | Direct A 可 step A；Gate/IRQ 拒绝 |
| `kcore_user_clone` [U:122](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export/user.rs#L122) | P | current user Task.owner=P；新 Task/AS 同 P | current reply 状态；深拷贝再 task_create | fork 策略在 personality；Core 管复制/生命周期 | Direct A；创建门禁仍适用 Gate/IRQ |
| `kcore_user_replace` [U:136](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export/user.rs#L136) | P | current 与 staged Task 都 owner=P | 当前 reply，staged Created/prepared；转交 UserDomain，旧域退役 | staged Task 移除；AS owner 不需新身份 | 同 owner 不变；不是组件替换 |
| `kcore_user_discard` [U:141](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export/user.rs#L141) | P | 目标 Created user Task.owner=P | 仅未启动 user Task；移除并释放未发布 backing | 不销毁运行中 user Task/组件 | owner 规则一致；不等于 Failed native 全回收 |

底层依据：[accessible](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/task/user.rs#L231)、
[copy](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/task/user.rs#L271)、[prepare/protect](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/task/user.rs#L310)、
[clone/replace/discard](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/task/user.rs#L434)。

### 3.10 其他（16）

| Core API / 实现 | 调用时 Principal 来源 | 操作对象 Owner 来源 | 使用的执行上下文 | 隐含假设 | 是否跨机制一致 |
|---|---|---|---|---|---|
| `kcore_trace_read` [E:359](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L359) | N/A | N/A | 环形记录快照复制 | 序号/容量；可信可写 out | K 身份无关；I import 不支持 |
| `kcore_trace_stats` [E:390](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L390) | N/A | N/A | trace 状态快照 | 不构成资源 grant | 同 trace_read |
| `kcore_now` [E:422](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L422) | N/A | N/A | 时间快照 | clock arch 后端 | K/I 支持，无 owner |
| `kcore_timebase_hz` [E:430](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L430) | N/A | N/A | 时间基准查询 | machine 提供值 | K/I 支持 |
| `kcore_console_write_byte` [E:314](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L314) | N/A | N/A | console 写 | 没有 per-component console owner | K/I 支持；输出本身不是隔离 |
| `kcore_log_line` [E:320](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L320) | N/A | N/A | 校验 ptr/len 后日志 | 字节可读，Core-critical 深度保护 | K/I 支持 |
| `kcore_machine_boot_hart` [E:412](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L412) | N/A | N/A | machine 查询 | 只读 | K/I 支持 |
| `kcore_machine_cpu_count` [E:436](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L436) | N/A | N/A | machine 查询 | 只读 | K/I 支持 |
| `kcore_machine_has_hart` [E:442](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L442) | N/A | N/A | machine 查询 | hart 标识不是 Task CPU owner | K/I 支持 |
| `kcore_free_page_count` [E:458](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L458) | N/A | N/A | allocator 统计 | 全局剩余，不是组件 quota | K/I 支持 |
| `kcore_task_count` [E:469](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L469) | N/A | N/A | TaskTable 统计 | 全局记录数量 | K/I 支持 |
| `kcore_component_count` [E:473](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L473) | N/A | N/A | Registry 统计 | 包含保留实例记录，不是可运行数量 | K/I 支持 |
| `kcore_panic_escape` [E:1034](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L1034) | active guard 或 cross-AS invocation | 故障归因 guard.owner；不是资源 owner 参数 | Core ABI depth>0 拒绝；IRQ 不可逃逸；Task/Init/Exit/Gate 分别恢复 | panic=abort，不 unwind；锁/裸指针安全不自动成立 | Direct 归 caller 边界；Gate 归 provider；I 有跨 AS 逃逸 |
| `kcore_console_read_byte` [Q:6](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export/query.rs#L6) | N/A | N/A | console 查询；无字节时 idle_wait 后 EAGAIN | 不是调度式 park；没有 owner | K；I import 不支持；不能从名字推定任何上下文都适用 |
| `kcore_component_nth` [Q:28](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export/query.rs#L28) | N/A | 仅观察 Registry.id，不授权 | registry 锁内复制值/名字 | 不导出 state 指针 | K；I import 不支持 |
| `kcore_endpoint_nth` [Q:73](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export/query.rs#L73) | N/A | 仅观察 endpoint.provider，不授权 | endpoint 锁内复制值/名字 | 不导出 api/ctx | K；I import 不支持 |

分类计数：3 + 9 + 4 + 3 + 4 + 4 + 5 + 2 + 11 + 16 = **61**。

## 4. 五种执行上下文的精确对照

下表的 B 默认为 KernelNative；I 的额外差异单列。必须同时观察 `Tcur` 和 `RequestContext.task`，
不能把“IRQ/Init 没有普通任务语义”误写成“CPU 一定没有 scheduler current Task”。

| 项目 | A：Create B | B：Worker B | C：A Task Direct B | D：A Task Gate B | E：IRQ B |
|---|---|---|---|---|---|
| principal / 来源 | B，Init guard；[load:219](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/load.rs#L219) | B，Task guard；[enter_task:1063](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/containment.rs#L1063) | A，普通调用不换 guard；[SDK:95](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/components/kcomp-sdk/src/block/backend.rs#L95) | B，ServiceCall guard；[service:721](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/containment.rs#L721) | B，route owner scope；[IRQ:134](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/irq/mod.rs#L134) |
| Tcur / Task owner | boot anchor 可无 Task；由 A Task create 时仍 A | B Task / B | A Task / A | A Task / A，绝不改 Task owner | 被打断的 Task 可仍存在，owner 可能 A |
| RequestContext.task | None | B Task | A Task | A Task，仅 provenance | None |
| 上述 task 依据 | [Init/IRQ task 编码](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/containment.rs#L257) | [Task owner](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/task/record.rs#L74) | [ambient fallback](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/context.rs#L38) | [caller_task](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/call.rs#L248) | [scope](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/containment.rs#L1024) |
| 栈 / AS | Core 临时 create stack；K boot AS | Task 自有 stack；K boot AS | A Task stack；K boot AS | K：Core per-call stack；I：B private stack/AS | trap/被中断执行链上的 stack；本 scope 不建立普通可调度 Context |
| 栈/AS 依据 | [run_isolated:945](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/containment.rs#L945) | [Task 创建](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/task/table.rs#L54) | [Direct](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/components/kcomp-sdk/src/block/backend.rs#L95) | [service stack](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/containment.rs#L721)、[I dispatch](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/isolated_lifecycle.rs#L448) | [IRQ scope](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/containment.rs#L1024) |
| memory / heap | memory 可用；K shared heap；I private HeapState | K 可用 | K 可用；没有“B heap 对象 owner” | K 可用；I memory 为 B AS/private heap | K 资源 API 可重入，不应把可分配等同于有延迟上界 |
| 内存依据 | [memory:157](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L157)、[heap:272](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L272) | 同左 | 同左 | [域选择](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L133) | [IRQ 能力](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/containment.rs#L1024) |
| claim / DMA alloc | B | B | 新 claim/alloc=A；B 已 claim 时 A 再 claim 不成立 | K：B；I import 不支持 device/DMA | B；只有受信 K，资源登记仍存在 B1 |
| device/map/free | mapping 从 device owner；free 按 allocation owner | 同左 | B device mapping=B；B claim/release/register 的 caller 检查不自动通过 | K B；I 不能部署硬件驱动 | B route 资源；unmap 不验 caller |
| 资源依据 | [claim](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L1125)、[alloc](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L1271) | 同左 | [map](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/dma.rs#L289)、[free](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/dma.rs#L276) | [I imports](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/isolated_load.rs#L315) | [IRQ register](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/irq.rs#L313) |
| 创建自己的 Worker | K Init 可创建 B Task；祖先 Gate/IRQ/Policy 仍禁止 | 可，entry 在 B image | 用 B entry 会因 A image 检查失败；A trampoline 创建的是 A Task | 禁止创建/启动 Task | 禁止创建/启动 Task |
| 创建依据 | [task:121](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/task/mod.rs#L121) | 同左 | 同左 | [祖先门禁](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/containment.rs#L594) | 同左 |
| 等待 / yield / park / exit | anchor 可 sched_run；若在 A Task 中直接 yield/park/exit 触发 B4 | 操作 B Task，可等待 | 操作 A Task，可等待 | 拒绝调度；不能等待依赖本 CPU IRQ 的完成 | 拒绝调度；不能阻塞当前 trap |
| 等待依据 | [run](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/sched.rs#L673)、[yield](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/sched.rs#L699) | [park](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/sched.rs#L719) | 同左 | [门禁](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/containment.rs#L594)、[IRQ masked](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/containment.rs#L731) | [门禁](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/containment.rs#L594) |
| unpark | 可唤醒同 owner B Task | B→B | A→A；不能唤醒 B Worker | K B→B 可以；不能唤醒 A caller | B→B 可以；不是调用 sched_run |
| unpark 依据 | [owner 检查/提交](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/sched.rs#L740) | 同左 | 同左 | 同左 | 同左 |
| endpoint publish | Init B 可以 stage | 禁止 | 禁止 | 禁止 | 禁止 |
| publish 依据 | [ambient_init](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/context.rs#L82)、[publish](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L610) | 同左 | 同左 | 同左 | 同左 |
| panic / failure | 返回 errno/panic→B Failed；不 destroy；扫资源、保留已暴露 backing | Task abort→B Failed，停止后续调度 | B 函数 panic 归当前 A Task boundary，A Failed；B 未必 Failed | provider B Failed，返回 transport error；A 可继续 | IRQ panic 无 resumable Context，致命，不能承诺只 fail B |
| failure 依据 | [create 结果](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/load.rs#L234)、[failure](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/failure.rs#L33) | [abort](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/containment.rs#L1168) | 同左；无 B guard | [complete](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/call.rs#L478) | [escape_target](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/containment.rs#L1225) |
| inflight / Stop | Init 不用服务 inflight；Starting 无法 Stop | live Task 就拒绝 Stop | Core 看不到每次 Direct 执行；非空 API publication 保活 | begin_call 与 Ready 验证同 Registry 事务，Stop 看见计数 | begin_irq 与 route 快照同 Registry 事务，共用 inflight；release route 后计数仍阻止 Stop |
| Stop 依据 | [stop:96](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/exit.rs#L96) | 同左、[live_tasks](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/task/table.rs#L99) | [has_direct_exports](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/endpoint.rs#L573) | [prepare](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/call.rs#L145)、[begin_stop](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/registry.rs#L189) | [prepare_callback](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/irq/mod.rs#L121)、[begin_irq](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/registry.rs#L266) |

Isolated 当前没有普通 Worker/IRQ/device/DMA imports。其 create/destroy/service 使用真实 private AS，
单实例服务调用以 `active_calls != 0` 拒绝重入/并发进入同一私有栈，不是多 Task 的 RPC server。
当前白名单是 19 项，不是全部 Core ABI。
依据：[imports](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/isolated_load.rs#L315)、
[单实例 admission](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/call.rs#L145)。

## 5. 真实 BlockDevice 链与四种部署

### 5.1 现有链路与资源发生时点

真实消费者链是 `FatFs disk_read → kcomp_block_read → Direct table.read → virtio_blk provider_read
→ VirtIOBlk::read_blocks → CoreHal::share → kcore_dma_map`。
Rust typed BlockDevice 前端也从同一 binding 选择 Direct/Gate。
依据：[FatFs](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/components/filesystems/fatfs/diskio_kaleidos.c#L73)、
[C 前端](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/components/kcomp-sdk/c/kcomp_block.c#L98)、
[Rust 前端](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/components/kcomp-sdk/src/block/backend.rs#L95)、
[驱动读](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/components/drivers/virtio_blk/src/lib.rs#L121)、
[HAL share](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/components/drivers/virtio_blk/src/hal.rs#L150)。

`virtio_blk` 在 B create 中 claim、记录 DeviceId、建立 VirtIO 队列并做 sector 0 自检，然后发布表。
队列 DMA allocation 在该 Init 中属于 B；稳态 read/write 的请求数据借自 caller buffer，header/response 在当前调用栈，
HAL share 为 B device 建立 mapping。不能把“Direct 中新 alloc 会归 A”写成“现有每次 read 都分配 A-owned DMA 队列”。
真实驱动当前轮询，没有注册 IRQ handler 或创建 Worker。
依据：[create](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/components/drivers/virtio_blk/src/lib.rs#L180)、
[队列建立/自检](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/components/drivers/virtio_blk/src/lib.rs#L282)、
[HAL alloc/dealloc](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/components/drivers/virtio_blk/src/hal.rs#L100)、
[HAL share/unshare](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/components/drivers/virtio_blk/src/hal.rs#L150)。

第三方依赖是 `virtio-drivers 0.13.0`，`default-features=false`：
[组件 Cargo 配置](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/components/drivers/virtio_blk/Cargo.toml#L15)。
本轮检查本地 Cargo 源码：该版 `src/device/blk.rs:177` 的 read_blocks 使用 request_read，
`src/queue.rs:311` 的 add_notify_wait_pop 以 `spin_loop()` 等 used ring，没有调用 task_park。
`src/queue.rs:259` 的间接描述符实现用 Box，不能未经依赖核查就认定是 CoreHal::dma_alloc。
该分支需要第三方 crate 的 alloc feature；本组件关闭默认 features，不能将此分支的存在当成本组件正在执行它的证据。
这些是外部依赖版本事实，不作为仓库内实现契约；升级依赖后须重审。

现有 S-mode Isolated **不能装载这个真实驱动**，因为它引用白名单外 device/DMA/heap 等符号。
因此 Case 2 以下区分“已实现的软件 BlockDevice Gate 形态”与“尚不支持的硬件服务”。
Case 3 的组件私有队列有 checksum Active/Hybrid fixture 证据，但不是已实现的 VirtIO Active driver。
Case 4 全是 D 层设计推演。不能用通用服务成功调用替代物理 I/O 的证据。
依据：[import 检查](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/load.rs#L353)、
[Isolated 白名单](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/isolated_load.rs#L315)、
[Active fixture](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/components/tests/kcomp_checksum/src/lib.rs#L54)、
[跨 owner wake 用例](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/components/tests/core_test/src/runtime/convergence.rs#L142)。

### 5.2 四部署、十五问题矩阵

| 问题 | 1 Native Direct：现有真实驱动 | 2 Isolated Gate：现有软件服务；硬件驱动不支持 | 3 Native Active Server：队列形态有 fixture；物理驱动待实现 | 4 U-mode Sandboxed Server：D 推演 |
|---|---|---|---|---|
| 1 实际 Task | A 的 filesystem Task 直接执行 B.read | dispatcher 用 A 的同步执行来源，非 B Worker；Tcur 仍 A | B Worker 执行设备业务；A Task 提交/等回复 | 预期 B 的独立受控 Task；尚无组件 server transport |
| 2 当前 AS | 共享 K boot AS | B dispatcher 期间 B private AS；Core trampoline 往返 | 共享 K boot AS，换 Task 不等于换 AS | B 私有 U AS；每次 syscall/trap 进入 Core；不共享裸 native 表 |
| 3 principal | 业务 B.read 中 Core API 仍 A | B；caller A 只保留 provenance | B Worker 为 B；A 调队列 submit 的 Direct 方法仍 A | 必须从 Core 已验证的运行域/Task 得 B，不能信任用户传 ComponentId |
| 4 Device owner | B create claim | 无 device import；不能 claim VirtIO。假设扩展后也须 Core 建 claim，不能视为现有事实 | 应在 B Init/Worker claim，归 B；不是在 A submit 中新 claim | 须 Core 仲裁 B 的设备访问；U MMIO window/代理尚未实现 |
| 5 DMA allocation | 队列在 B Init 分配归 B；若另在 Direct 中新 alloc 则 A | 无 DMA import；memory backing/私有 heap 可用，无 DMA allocation owner 可填 | B Init/Worker 分配归 B；A 请求 buffer 另保留来源 | 未来受控 allocation 归验证后的 B 或合法借入资源；需要 backing/访问校验 |
| 6 DMA mapping | device owner B；可映射 A 借入 buffer | 无 DMA path；普通 Gate buffer 不等于 DMA map | device owner B；借入 A buffer 要维持完成前存活 | B 已 claim 设备 + 来自合法 backing 的受控 mapping；需 pin/撤销条件 |
| 7 IRQ handler | 当前真实驱动没有；若 B 注册则 route=B | 无 IRQ import；不能用本 Gate 等设备 IRQ | B 注册；IRQ scope=B，可唤醒 B Worker | Core 路由到受控 B 执行/通知；不能直接跳 U function pointer |
| 8 分配资源 | 可以，但 new alloc/task/claim 用 A principal；实例状态变更由 B 锁保护 | 可 B memory/private heap；设备/Tasks 不支持；部分 Native Gate 资源 API 可用 | B Worker 可分配自己资源；submit 方法按其实际边界决定 | 只能经已实现的受控入口；不能调用任意 native allocator/MMIO API |
| 9 等 I/O | 可以阻塞 A Task；真实实现忙等轮询。若改 park，需要 A-owned 通知来源 | Gate 不可 yield/park，native service 整段 irq-save；纯计算/有界 polling 可成立，依赖本 CPU IRQ 的等待不成立 | B Worker 可 park；B IRQ 可 unpark B。A→B wake、B→A wake 都不能直接用现有 unpark；fixture 用 polling/yield | 需要未来受控通知/等待；没有现成 U组件同步 server 保证 |
| 10 建自己 Worker | 应在 B Init 建；A 直接执行 B 内 task_create(B.entry) 被 image 检查拒绝 | 当前 I不支持 Tasks；K Gate 同样禁止 task_create/start | B Init/Worker 可创建 B Tasks | 受控 B Task 创建/调度能力待定义；不由用户自报 owner |
| 11 Request / Buffer | SDK 同步借入 A buffer；B 私有队列归 B；Core 不记通用 buffer owner | K→I frame 在共享 Core 可见区域；I outbound 先复制 flat bytes。只借本次调用，不交私有指针权限 | 请求对象/队列生命周期由组件协议；A buffer 借入至确认完成/取消，或 B 私有复制；Core不管理 malloc/request | 跨 U 指针必须校验/marshal；共享 backing 必须 Core 已建立映射，生命周期显式闭合 |
| 12 B失败 A继续 | Result 可回 A；B panic 无独立 boundary，通常 A Task/组件 Failed；裸指针/锁没有故障隔离 | dispatcher panic→B Failed、A 可得 transport error；I 仍 S-mode，恶意内存行为无保证 | B Worker panic 可使 B Failed；A Task 未被同栈 panic 归因，但队列必须有错误/超时出口，共享锁/backing仍有风险 | 预期 U fault 停 B，A继续；仍要定义设备静默、请求失败通知和 Core故障边界 |
| 13 谁 Stop B | 健康 K caller可请求；B发布非空Direct表，实际 EBUSY | 健康 K caller可请求；B无 live tasks/inflight且Ready则可；I caller没有 stop import | K caller可请求；B Tasks全Exited才可；若还有Direct表则仍EBUSY | 未来由已授权管理者；不能把“任意U传B id”当权限 |
| 14 安全回收 | B-owned DMA进quarantine，设备quarantine，route/maps逻辑撤销；已暴露image/state驻留；A借入buffer不会自动pin | AS逻辑退役、已知临时ABI/桥接窗口受控释放；image/private heap/页表驻留；精确动态memory_release需无借用 | 同K；正常Worker退出不自动回收组件；无任务≠设备已静默；借入A backing不能推定安全free | 真实访问撤销+所有CPU离域+TLB处理+DMA静默/隔离之后才可完整回收；尚未实现 |
| 15 业务代码可不变 | 当前Hal+同步驱动可用；长期资源初始化与caller临时资源应分开 | 扁平Block contract/纯计算后端可复用；真实VirtIO驱动不能只换domain运行 | 解码/设备算法可复用；队列、通知、取消、借用期限与等待方式需要明确改变 | 协议/算法可复用；特权HAL、transport、装载、指针/资源处理不能原样复用 |

各列实现依据分别是：

- Case 1：[FatFs read](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/components/filesystems/fatfs/diskio_kaleidos.c#L73)、
  [provider lock/read](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/components/drivers/virtio_blk/src/lib.rs#L121)、
  [HAL](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/components/drivers/virtio_blk/src/hal.rs#L100)、
  [Direct Stop 保活](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/exit.rs#L96)、[failure](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/failure.rs#L49)。
- Case 2：[binding 选择](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/endpoint.rs#L287)、
  [I lifecycle/dispatch](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/isolated_lifecycle.rs#L212)、
  [I outbound 校验复制](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/isolated_call.rs#L111)、
  [释放受控窗口](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/isolated_lifecycle.rs#L640)、
  [imports](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/isolated_load.rs#L315)。
- Case 3：[组件私有 Worker/队列](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/components/tests/kcomp_checksum/src/lib.rs#L54)、
  [消费端](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/components/tests/core_test/src/runtime/convergence.rs#L113)、
  [unpark owner 提交](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/sched.rs#L740)、
  [Task abort](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/containment.rs#L1168)。
- Case 4：现有实现仅提供边界参照：[U step/trap](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/task/user.rs#L348)、
  [Sandbox load 拒绝](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/load.rs#L328)、
  [Sandbox bind 拒绝](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/endpoint.rs#L287)。本列的设备/通信/回收描述不是已实现行为。

### 5.3 DMA alloc 与 map 的不同 owner 是否矛盾

设 B 在 A Task 的 Direct 方法内新 `dma_alloc(x)`，再 `dma_map(B.device, x)`：
allocation.owner=A，mapping.owner=B。A principal 可 free allocation（实际 quarantine）；
B Worker/Exit free 会 EACCES，任何可信 Native caller 都可凭 mapping id unmap。
B Failed 时移除 B mapping，却不会匹配到 A allocation；A Failed 时 allocation 进 quarantine，
B mapping 不一定被移除，backing 也不会复用。改成 **KernelNative 显式 Gate** 后新 allocation.owner=B，
map.owner仍B。当前不能把这个实验直接改成 Isolated 物理驱动。
依据：[allocation/free](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/dma.rs#L265)、
[map/unmap/revoke](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/dma.rs#L289)、
[record/revoke](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/dma.rs#L210)。

这本身是**有意设计**：设备映射与 device-agnostic 分配是两个生命周期。
如果 x 是 caller 临时 backing，归 A 合理；如果 B 把 x 存入长期驱动状态却以后由 B teardown，
则实现假定“执行B函数就获得B principal”，与真实 Direct 语义不符。
应先在 B Init/Worker 分配长期资源，或显式走已有 provider Gate；不能从函数地址/ctx 偷换 owner。
若真实工作负载必须在零 Core Direct 中动态增加 B-owned 长期 DMA资源，现有 alloc ABI表达不足，
才应评估 §9C 的窄 device-related 分配入口。

### 5.4 哪些差异无法透明统一

| 差异 | 原因 | 应放在哪里 |
|---|---|---|
| Direct 保留 A Task/栈，Gate 有 provider stack/边界，Worker 有 B Task | 执行上下文 | 运行时调用契约；不由 Block 返回值掩盖 |
| Gate 不可 park/yield，native Gate 整段 irq-save | 当前同步 Gate ABI/实现限制 | 对接口的合法执行条件作清楚说明；未来异步方案另有真实需求再评估 |
| Direct alloc=A、Gate/Worker alloc=B；mapping都可归B | 资源归属 | Core acquisition 分类；业务区分临时借用与provider长期资源 |
| 两层 Gate status，Direct 单方法返回；跨域 flat marshal | ABI/传输 | SDK做现有编码与结果映射，不需要通用RPC Runtime |
| U不能访问裸native表/MMIO；S私有AS不能防恶意S代码 | 安全模型 | loader/Arch/Core必须验证和提交，不能靠Transport Adapter补齐 |
| “阻塞到完成”可能是polling或真正park | 接口执行语义目前模糊 | Block contract下一轮明确；不能称所有同步Gate都不能完成I/O |

[Block contract](../../abi/block.toml#L29) 同时说同步/阻塞、task-only，SDK 又支持 Gate。
它需要明确允许的“task 上下文”是 caller provenance 还是可调度Task，以及是否容许有界、不依赖本CPU IRQ的polling。
当前 Gate 内无法 park是明确代码事实；“所有Block Gate都已正确满足阻塞契约”没有证据。
业务接口可同为同步read/write，但相同内部等待代码、panic后果、owner和二进制部署能力无法全部透明不变。

## 6. 最少的归属规则与 Core 分工

| 来源 | 适合的操作 | 不适合/须限制 | 对 Direct、跨域、回收的影响 |
|---|---|---|---|
| Ambient execution identity | 新Task、新device claim、device-agnostic DMA allocation、Init publication | 不能推定当前函数provider；不能仅凭用户自报id | Direct沿caller；provider长期资源在Init/Worker；跨域从Core已验证入口取principal |
| Resource-intrinsic owner | device相关mapping、endpoint provider、IRQ投递归属、针对既有对象的teardown | lookup owner不自动给caller任意权力；必须另外验证操作的合法来源/lifecycle | 当前K Direct可map B device；未来私有域必须校验backing可达性和设备权限 |
| Explicit existing reference | TaskId、DeviceId、EndpointId、mapping id、MemoryView、ASHandle（Core内部） | 显式ComponentId不是授权；raw pointer不是跨域capability | 定位与授权分开；K受信unmap id可用，U transport不能照搬 |
| Provider-owned execution | B状态长期资源初始化、B Worker处理I/O、显式provider Gate | 不能要求所有B方法都切到B Task；Gate不能变成可阻塞worker | 可以保持Direct低开销；需要不同初始化/等待适配，但不需要新增身份对象 |

推荐的统一规则不是“所有API都用ambient”，而是：

1. **创建全新、无父资源的有主对象，owner取已验证的principal；与现有父资源相关的对象可继承父owner。**
   对每个公开入口写明这一选择，Memory/Heap无对象账本保持例外的明确事实。
2. **定位、归属、操作许可分开。** 显式id定位记录，Core按真实执行域/对象状态验证操作；
   KernelNative的协作记账和未来U的强制权限都不能由一个可伪造OwnerId代替。
3. **当前Task owner固定；provider boundary只改principal/栈/AS，不改Task归属。**
   caller_task只保留provenance。调度许可由完整边界链决定，不能仅凭“有Tcur”。
4. **新资源授权必须和owner lifecycle复验、表插入形成同一提交边界。**
   若caller与intrinsic owner不同，两者相关门禁都要复验；先分配后提交可以，但拒绝时释放未发布lease。
5. **逻辑Failed之后不再获得新authority；允许有明确对象依据的清理。**
   内存无owner/共享heap的既有受信契约单独说明，不承诺逐malloc撤销。
6. **公开服务被缓存的裸表/ctx存在时保留驻留；inflight只覆盖Core已准入的执行。**
   Failed不等于quiesced，不等于全CPU停止，不等于DMA静默。
7. **IRQ回调有provider归属，没有普通Task调度能力。** 它可做受控非切换wake，不能park/yield；
   route撤销与已准入callback完成是两个事件。
8. **执行权限与业务等待条件属于接口的合法使用契约。** Core负责保护全局不变式，
   queue、请求取消、文件系统语义、设备协议与锁保护由组件维护。

这些是下一轮建议；需分别落实到原有权威文档，不能将本页变成第二份资源契约。

## 7. 问题清单、分类与最小复现

### 7.1 总表

| 编号 | 唯一分类 | 判断 / 优先级 | 证据 |
|---|---|---|---|
| B1 | Confirmed Bug | lifecycle check与resource commit分离，可在Failed撤销后重新登记authority / 高 | S/I，尚无本轮SMP复现 |
| B2 | Confirmed Bug | IRQ enable/disable持IRQ相关spin锁时未irq-save，可同CPU trap自锁 / 高 | S/I，尚无本轮硬件复现 |
| B3 | Confirmed Bug | route变化与控制器enable/disable不组成串行提交，旧操作可覆盖新route / 高 | S/I，尚无本轮硬件复现 |
| B4 | Confirmed Bug | Task嵌套Init/Exit可以yield/park/exit，恢复时丢失生命周期guard / 高 | S/I，尚无本轮真实切栈复现 |
| B5 | Confirmed Bug | DMA mapping id回绕违反永不复用，旧id可指向新mapping / 低 | S；u64耗尽边界，不是常规负载故障 |
| B6 | Confirmed Bug | create/load未拒绝Failed caller，失败远端执行仍可建立新实例/work / 中 | S/I；未在本轮SMP复现；其他上下文许可另见S3 |
| S1 | Semantic Inconsistency | Block“task-only/阻塞”与受限Gate的等待条件未明确 / 中 | S，§5.4 |
| S2 | Semantic Inconsistency | K/K bind只选Direct，Gate-only provider的显式call存在但普通bind不能表达 / 中 | S |
| S3 | Semantic Inconsistency | 部分操作的上下文许可按子系统分散：Policy资源分配、IRQ重加载等没有完整矩阵 / 中 | S；未断言所有这些操作都必须被禁止 |
| L1 | Intentional Limitation | Direct不换principal，alloc/map不同owner；调用B不等于获得B资源权限 | S，§4/5.3 |
| L2 | Intentional Limitation | K共享特权/AS，Direct panic归caller；驻留/保活不等于撤销裸指针 | S |
| L3 | Intentional Limitation | Gate不能调度；IRQ不可阻塞且panic致命；unpark须同owner | S/T（逻辑）；非硬件证明 |
| L4 | Intentional Limitation | Stop要求无live Tasks/inflight；Direct publication即保活；Failure不destroy | S/T（逻辑） |
| L5 | Intentional Limitation | DMA free永远quarantine；IRQ failure仅删route，不保证controller关线 | S/T（quarantine逻辑） |
| F1 | Future Gap | 私有域设备/DMA/IRQ/Worker及U `.kcomp` service transport未实现 | S/D |
| F2 | Future Gap | 借入buffer pin/设备静默、远端CPU退域/TLB与完整物理回收未闭合 | S/D |
| F3 | Future Gap | 跨owner请求通知/取消/死亡回复没有现成Core原语，Active fixture只证明组件私有队列 | S/D |
| M1 | Documentation Mismatch | 模块页44项/旧I白名单，testing说无stop ABI，均与现状不符 | S |
| M2 | Documentation Mismatch | dma_map源码注释说caller须device owner，生产实现/驱动契约明确不检查 | S |
| M3 | Documentation Mismatch | route/release注释宣称此后无callback进入，遗漏已经准入但未开始的callback | S/T（计数逻辑） |

### 7.2 B1：资源获取与撤销必须有共同提交边界

路径：export先`deny_if_failed(P)`，释放Registry锁，再在各表提交；
`fail_component`提交Failed后释放Registry锁，顺序扫IRQ、DMA、device、endpoint。
资源层claim/alloc/register/map没有同事务复验owner的可授予状态。
依据：[deny_if_failed](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L876)、
[alloc入口](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L1271)、
[alloc提交](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/dma.rs#L265)、
[claim提交](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/device.rs#L236)、
[register提交](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/irq.rs#L313)、
[failure sweep](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/failure.rs#L33)。

一个最小SMP交错是：

```text
CPU0：B-owned Worker调用dma_alloc，caller Failed检查通过；暂停在表插入之前。
CPU1：另一个B Worker panic；mark_failed(B)，DMA sweep完成，整个failure返回。
CPU0：继续插入Allocation{owner:B}；返回成功。
结果：Failed(B)拥有一个failure sweep永远看不到的新allocation。
```

同类claim可在device sweep后取得新设备；register可在IRQ sweep后、device sweep前插入新route。
map还有不同owner的窗口：A调用B device的map，caller A健康，B已Failed；
在B DMA sweep之后、device quarantine之前，device表仍显示owner B，map可插入B-owned mapping，
随后device失去owner而mapping残留。这里验证A不足，必须复验intrinsic owner B。

UserDomain也有同类先查后提交路径：`export/user.rs::owner()`释放Registry锁后，
`user_map`才进入Task表锁并向目标AS加映射；Created且同owner的Task仍可通过accessible检查，
其间owner变成Failed不会被这个提交点重新检查。下一轮应把这一授权提交纳入B1覆盖，
而不是只修三张硬件资源表；UserDomain的逻辑失败与物理驻留仍不能被误解为新映射许可。
依据：[User owner检查](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export/user.rs#L6)、
[User map入口](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export/user.rs#L40)、
[User accessible/map](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/task/user.rs#L231)。该真实实现路径需要rv64 S-mode/MMU测试，
本轮host测试没有运行它的硬件后端。

这不是把“所有table必须同时加一把大锁”当解决办法：可在registry→resource的既有锁序下复验与插入，
使Failed提交与每个新grant互斥；grant在Failed前完成的对象由后续sweep捕获，Failed后不再插入。
需要先列全锁序和所有内层Registry查询，避免resource→registry反向锁。
内存分配可先取得未发布lease，再在提交点复验，拒绝时drop；不要持Registry锁做组件调用。

最小复现：下一轮host测试用test-only barrier控制真实admission/commit，
两个线程分别grant与fail，检查Failed后device/IRQ/DMA表没有新live owner记录、未发布lease未泄漏。
另用两个真实B Tasks在双CPU CoreTest中确认API结果；CoreTest只经公开ABI，不改私有表。
现有release-map/device-child锁事务已经修复，不能把它重新报告为本轮未修问题：
[device release](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/device.rs#L277)、[DMA map](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/dma.rs#L289)。

### 7.3 B2：Core-critical并不等于屏蔽IRQ

`resource::irq::enable/disable`拿普通`spin::Mutex`的device和IRQ表锁，函数本身没有IrqSaveGuard。
export包装的`with_core_critical`只改变panic escape深度，并不屏蔽中断。
BSP普通Task持IRQ表锁时，外部IRQ可进入`prepare_callback→route`再拿同一IRQ表锁，导致同CPU自旋死锁。
Register/release路径有local irq-save；RegistryLock也有自己的irq-save，但此前短暂的caller检查结束后会恢复IRQ，
不能保护后面的resource表临界区。
依据：[enable/disable](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/irq.rs#L346)、
[普通Mutex](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/irq.rs#L251)、
[Core-critical](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/containment.rs#L404)、
[IRQ路由](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/irq/mod.rs#L121)、
[Registry guard](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/registry.rs#L334)。

最小复现：ArchTest/QEMU在BSP注册并使能一条可反复产生的IRQ，普通Task重复enable/disable；
用测试层控制在持表锁期间使IRQ pending，预期旧实现可停在自锁，新实现完成且恢复调用前IRQ状态。
host只能检查临界区入口irq-save discipline，不能证明硬件trap不会重入。
修补不能仅把硬件写搬到锁内而保留IRQ开启状态。

### 7.4 B3：旧硬件操作可以覆盖新route

当前release在device→IRQ锁内删route，释放锁后才写controller disable；enable先验证route，释放锁后写enable。
存在同owner、同device的合法交错：

```text
CPU0：release old route，移除后解锁，暂停在硬件disable前。
CPU1：register new route；enable new route并完成controller enable。
CPU0：恢复，controller disable。
结果：live new route被旧release关断；Core表与硬件投递状态不符。
```

逆向也可发生：CPU0 enable验证old route后暂停，CPU1 release并disable，CPU0旧enable再开无route的line。
local irq-save不能阻止另一CPU。失去的不是ComponentId，缺失的是验证/route变更/controller动作的线性化顺序。
依据：[enable](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/irq.rs#L346)、
[disable](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/irq.rs#L366)、
[release](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/irq.rs#L389)。

最小复现：host用Arch fake controller记录真实Core路径的MMIO事件，以barrier固定交错；
检查最终live route与最后合法enable/disable操作一致。随后QEMU双CPU验证真实line投递。
下一轮选择窄临界区串行化controller提交，或同一条线的受控硬件操作锁；必须遵守irq-save和全局锁序，
不跨组件回调持锁。controller写是Core必要机制，可为正确性接受有界MMIO临界区。

Failure `revoke_owner`刻意只删route、不关controller，是L5；不能把这个已写明的限制与上述旧操作覆盖混在一起。

### 7.5 B4：嵌套生命周期栈可以误用caller Task的调度入口

合法入场路径是A Worker调用`component_create(B)`或`component_stop(B)`，
Core建立B的Init/Exit临时栈；scheduler.current仍是A Task。
`scheduling_forbidden()`只检查IRQ/Service/Policy祖先，未拒绝Task上方的Init/Exit。
B的yield/park/exit于是操作A Task。
当切到一个Task时`enter_task`覆写每CPU的task_guard并令active指向它；
Task区域已激活，新的Init/Exit guard不会保存为anchor。
切回A后scheduler仅恢复Core ABI depth，不恢复被挂起的B lifecycle active guard。
依据：[创建包装](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L558)、
[独立生命周期栈](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/containment.rs#L924)、
[调度门禁](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/containment.rs#L594)、
[yield/park/exit](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/sched.rs#L699)、
[调度切换](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/sched.rs#L625)、
[enter_task](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/containment.rs#L1063)。

后果是B lifecycle恢复执行却principal=A；`ambient_init`不再识别B，发布失败；
若再panic，归因可能落到A Task而非B Init/Exit。
若B直接task_exit，A不再回来，loader恢复creating、create/stop收尾都可能被遗弃。
这不是允许Direct阻塞的正常后果：Direct没有Core额外生命周期guard；这里建立了guard却没保存/恢复。

最小复现：测试组件A有两个Worker；其中一个create B；B.create先publish一个sentinel，
调用task_yield后再次publish另一个sentinel，然后正常返回。
预期合法实现要么在yield处明确拒绝，且两个publication仍归B；要么完整支持切换并恢复B guard。
旧路径允许yield后，第二个publication可因没有Init身份返回EPERM；再加panic变体检查错误归因。
用QEMU真实Task/context_switch验证；host已有nested-init恢复测试不含实际调度，不能代替该复现。

最小修补建议是仅允许真正的Task执行边界调用yield/park/exit，
不要一律禁止Init的所有调度操作：boot/anchor Init中的`sched_run`与创建/启动B Worker是现有CoreTest的正常用法。
若选择支持生命周期栈挂起，则必须保存完整per-Task guard链/creating状态并解决临时栈期限，
这比拒绝此狭窄上下文明显更复杂，本轮没有需求证明值得这样扩展。

### 7.6 B5、B6与模糊规则

**B5**：[insert_mapping](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/dma.rs#L175) 使用
`next_id.wrapping_add(1).max(1)`，u64耗尽后重新发出1；与驱动契约§6.3的“永不复用”相冲突。
旧id可能删除新mapping，或出现重复live id。下一轮test模块从near-max初始化私有table，
检查耗尽拒绝且未改变table；不用跑2^64次。ComponentId和EndpointId已有耗尽拒绝路径，
不需为此增加generation对象。TaskId也有有限宽度分配，但本问题的明确契约与生产违规证据是DMA mapping，
不将未经具体可达性分析的所有整数计数器一概归为同一故障。

**B6**：create/load既不解析caller也不检查Failed，load内部只拒绝Policy祖先。
共享K允许健康受信组件管理别的实例是合理信任模型，不需要parent/child权限账本。
但另一个CPU上的B仍可能在B Failed后继续create C，违反Registry已经明确的“失败实例不得创建新work”门禁。
交错为CPU1提交Failed(B)并撤销资源，CPU0的B Worker尚未到调度边界，调用create(C)；
入口完全不检查B状态，C可进入Starting/Ready。这不同于B1的先查后提交竞态，是入口遗漏检查。
下一轮应明确Core内部无ambient调用与组件ABI调用的区别，建立caller/context门禁；
再补Failed caller、IRQ/Gate祖先、合法嵌套Init的错误路径测试。
依据：[load dispatch](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/load.rs#L163)、
[create/load wrappers](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L529)、
[stop wrapper](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L481)、
[失败新work门禁](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/registry.rs#L235)、
[失败不强停远端Task](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/failure.rs#L18)。

**S2**：K/K bind选择Direct并要求api非空；native dispatcher-only endpoint却可以经显式
`endpoint_call`使用，checksum Gate-only fixture就是该形态。
不能删除Direct或添加Transport Adapter来掩盖它；下一轮先明确是否保留“discover+显式call”的既有使用方式，
还是需要让bind依据provider入口形态选择Gate。后者影响binding语义但未必改ABI布局。
依据：[select](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/endpoint.rs#L287)、
[bind](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/endpoint.rs#L514)、
[fixture publication](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/components/tests/kcomp_checksum/src/lib.rs#L130)。

**S3**：祖先门禁覆盖调度和通用服务；它不是所有Core API的能力矩阵。
例如Policy不能创建组件/Task或调用通用服务，但memory/heap/resource接口没有统一“Policy不得分配”检查；
IRQ可以调用create/load，甚至进入耗时加载；User map/protect等对Created owner Task允许修改，
不全部按Service/IRQ/Policy上下文禁止。API可返回成功，不代表满足非阻塞/有界执行的运行契约。
下一轮应逐项明确“可修改既有对象”“可分配”“可等待”“可发起加载”，并测试必要门禁，
不建立通用ExecutionContext registry或夸大现有门禁为全面沙箱权限系统。
依据：[policy chain](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/containment.rs#L612)、
[memory/heap](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L157)、
[User accessible](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/task/user.rs#L231)、
[User map](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export/user.rs#L40)。

### 7.7 明确接受的限制

L1–L5不需要为了形式统一而修成“所有调用自动获得provider身份”：

- Direct稳态数据调用不进Core，保留caller Task/principal；新长期资源放provider Init/Worker，
  需要provider边界时已有显式Gate。Device owner mapping/unmap允许可信Direct借入buffer。
- K共享特权不防恶意组件，也无法从Direct pointer撤销证明热卸载。
  Stop在发布非空表时即拒绝，甚至endpoint已Invalid仍拒绝；不追踪是否真的bind或每次调用。
- Gate/Policy/IRQ不建立可调度的独立Task。IRQ可同owner unpark，但不可将被打断Task当其可阻塞任务。
  Native Gate通常整段IRQ屏蔽，不能把睡眠驱动原样置入Gate。
- Task退出、Component Stop、Failed是不同事件。失败停止后续调度，远端当前Task尚需协作到边界；
  Stop不负责drain live Tasks，failure不调用destroy。新实例不是复活旧id，也不解除设备quarantine。
- DMA free只quarantine；map/unmap不证明设备已经停止访问。Failure删IRQ route不mask硬件线，
  可能继续收到无人投递的IRQ；控制器/设备静默与owner撤销不能混淆。

实现依据：[Stop](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/exit.rs#L96)、
[Direct publication](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/endpoint.rs#L573)、
[scheduler owner死亡复验](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/sched.rs#L569)、
[quarantine](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/dma.rs#L237)、
[IRQ revoke](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/irq.rs#L421)。

### 7.8 Future Gap与文档差异

F1：I白名单尚未包含设备/IRQ/DMA/Tasks，Sandbox create与bind都拒绝。真实U用户程序可执行，
但U组件实例装载、服务调度、syscall资源授权和通知不是已实现能力。

F2：Mapping只存id/owner/device，不存buffer extent或pin；普通caller heap/stack/memory_release不会因B mapping自动驻留。
Gate flat复制只解决CPU指针表示，不解决设备对借入buffer的存活。
Init临时Core stack在返回/panic后释放，native service panic stack则保留；
若把Init栈buffer映射给未静默设备，failure扫mapping也不证明可以回收该栈。
这是当前可信借用契约与缺失pin关系的边界，不能声称所有可DMA访问backing已被自动保护。
完整私有域回收还需所有CPU不再执行、映射撤销/TLB处理、DMA静默或隔离证明。
依据：[Mapping记录](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/dma.rs#L119)、
[Init stack释放](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/containment.rs#L924)、
[service panic保留](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/containment.rs#L721)、
[精确memory release](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L228)。

F3：组件私有队列可以保持简单，但不能假定跨owner unpark已存在。
A与B的等待/取消/失败回复可以先用组件协议与合法owner的notifier，或明确接受polling/yield。
若真实Active I/O负载证明必须有受控跨owner通知，才单独提出最窄原语；不据fixture就新增IPC Framework。

文档差异下一轮只修权威或现状的正确出处，不在本轮改契约：

| 编号 | 当前描述 | 源码事实/待改位置 |
|---|---|---|
| M1 | [组件模块页:67](../modules/components.md#L67) 为44项，I只有诊断/查询；[testing:147](testing.md#L147) 说没有stop ABI | ABI61项；I19项且有memory/endpoint；已有stop。更新模块事实与测试指南 |
| M2 | [export dma_map注释:1311](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L1311) 说只有device owner、非owner EACCES | [resource map](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/resource/dma.rs#L289) 不验ctx；驱动契约§6.3明确允许K Direct consumer。更正源码注释/错误列表，保留契约语义 |
| M3 | [route:140](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/irq/mod.rs#L140) / [release:1245](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/export.rs#L1245) 暗示撤销后绝无新callback执行 | [prepare_callback](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/irq/mod.rs#L121) 已复制并准入的callback可以稍后进入；release阻止后续准入，不drain已准入callback；inflight保护Stop |

概念页中的示意shutdown/drop代码不能覆盖冻结生命周期契约，更不能当成现有物理回收事实。
[component-model §4.9](../architecture/component-model.md#L437) 有示意流程；真实failure以
[failure.rs](https://github.com/Pan-Peach/KaleidOS/blob/69a3b436d830e8902dff428f361e0e0db05f8a0b/os/core/src/component/failure.rs#L33) 为准。本轮历史报告的QEMU结果亦不作为本轮重跑证据。

## 8. 哪些复杂性应保留，哪些特例可以收窄

必须保留的机制复杂性是：真实CPU Context保存、跨AS入口和指针搬运、全局device ownership、IRQ路由与admission、
设备可达backing生命周期、Core对policy提案的最终复验。业务队列、协议、服务锁不因此移入Core。

可以接受的部署限制是：Direct驻留不热卸载、Gate不可阻塞、S私有AS非恶意代码安全边界、U不能裸Direct、
特权Hal不能无修改进入U。这些限制比伪造统一执行语义更清楚。

可收窄的特例如下；“删除候选”均要先验证调用点，而非本轮已删除：

| 候选 | 收敛办法 | 必须保留的事实/条件 |
|---|---|---|
| `current_task_requester`名称误导 | 改名为current_principal等简单内部名，统一从RequestContext解析 | Service/IRQ的P未必等于Tcur.owner；不改ABI |
| `creating` fallback与Init guard两条归属通道 | runtime-init/create内部显式接收owner，正常入口只从guard解析；审计无boundary调用点后移除ambient的loader fallback | loader嵌套恢复与host/Arch fixture仍需真实验证；不能只删creating字段 |
| 多处只检查Failed的grant包装 | 在真实提交点按对象类别复验lifecycle；共享小辅助函数/清楚锁序即可 | teardown与无owner heap/memory语义不被错误禁止；不造通用资源trait |
| “有Tcur即可yield/park/exit”隐含规则 | 明确真正可调度Task边界；Init/Exit暂不支持挂起 | 保留anchor Init sched_run与初始化创建Worker |
| K/K Gate-only只能绕过普通bind的未说明用法 | 先明确显式call是否正式合法前端；确有需求再用已有dispatcher信息选择Gate | 不删除Direct，不新增RPC层；可能需要调整binding契约说明 |
| IRQ表和硬件操作分离的竞态规避注释 | 为必要硬件提交建立有界串行临界区 | 同CPUirq-save、SMP锁序、锁外组件callback都仍必须成立 |

以下规则**不是删除候选**：device-intrinsic DMA mapping；Native unmap不靠ambient；
无通用Memory owner账本；caller_task只作provenance；Direct非空API保活；祖先感知调度门禁。
它们分别表达不同事实，不能因为看起来不像统一ambient方案就抹掉。

## 9. 最多三种收敛方案

### 9.1 A：最小修补

保留现有身份/部署/接口选择，修B1–B6；创建入口的其他上下文许可先明确，再实施必要门禁。

| 项目 | 方案A |
|---|---|
| 必须涉及文件 | `component/export.rs`、`export/user.rs`、`load.rs`、`containment.rs`、`sched.rs`、`registry.rs`（按需小辅助）、`task/user.rs`、`resource/device.rs`、`irq.rs`、`dma.rs`、`irq/mod.rs`；各原有test模块及CoreTest/ArchTest最小fixture；修M1–M3原描述 |
| Core ABI | 不改签名/布局；DMA id耗尽通过既有errno返回；确定上下文错误码 |
| 新运行时状态 | 原则上不加owner/执行对象；IRQ若选独立controller提交锁，仅增加必要锁，不加第二route真相 |
| Direct性能 | 数据面不加Core调用；只有Core资源API多一次提交点复验 |
| Gate | 保留独立stack/inflight/nonblocking；B4修生命周期误调度，不让Gate自动阻塞 |
| SMP | Registry→资源提交，IRQ硬件事务，补固定交错测试；严查锁序与不可重入条件 |
| Sandbox | 不增加支持，保留unsupported；为未来受控grant准备清楚提交边界 |
| 新不变量 | Failed后无新grant；IRQroute/硬件动作有顺序；生命周期临时栈不能冒充Task；mapping id永不复用 |
| 可删特例 | 失效的irq锁外硬件正确性注释、零散先查后提交grant假定；不会自动删creating或改变alloc owner |
| 最大风险 | 生命周期门禁误伤CoreTest anchor init；registry重入/锁序倒置；MMIO临界区过长 |

### 9.2 B：语义收敛（推荐）

以A为前置，采用§6规则。把当前能力写成少量明确的执行/对象不变量，
明确接口在Direct/Gate/Worker下的等待条件与资源期限；没有真实需求就不增加API。

| 项目 | 方案B |
|---|---|
| 必须涉及文件 | A涉及文件；`resource/context.rs`（命名/身份入口）、`component/endpoint.rs`（明确Gate-only路径）、SDK `block/backend.rs` 与C前端注释；权威 lifecycle/deployment/driver/scheduling/memory文档，`abi/block.toml`仅在批准明确执行契约后原地修说明 |
| Core ABI | 保持61项签名与结构；改变context许可/绑定选择时按契约管理行为变化；不擅自兼容陈旧ABI别名 |
| 新运行时状态 | 不增加身份对象/registry；优先复用边界种类、Task owner、Device owner、ASHandle；creating删减需验证 |
| Direct性能 | 保留现有table调用零Core边界；provider长期资源放Init/Worker，不自动scope每次Direct |
| Gate | 保留provider归属、caller provenance、inflight与受限调度；明确native polling/IRQ条件；I仍单实例入口栈 |
| SMP | 同A，语义规则在最终commit验证；provider failure与caller admission分别说明，不假定local irq-save是跨CPU屏障 |
| Sandbox | 规则可复用，transport不能复用可信裸指针ABI；明确U Task不是已实现的U组件 |
| 新不变量 | §6的八条；区分临时caller backing与provider长期state；每个接口明确合法等待/并发/借用期限 |
| 可删特例 | requester误导名；经过核查的ambient loader fallback；“函数属于B所以API属于B”的隐含解释；重复非权威说明 |
| 最大风险 | 只改术语却不修grant commit；把非blocking等同不能同步完成；对既有fixtures/SDK Gate-only行为无意改变 |

推荐B是因为现有Component/Task/AS/Device已表达了实际对象关系；真实bug大多是提交边界和上下文许可问题。
A能止住错误，但留下业务作者容易误解的资源获取/等待规则；B不需要扩展框架就能消除这些隐含假设。

### 9.3 C：仅在真实需求证明时做一个窄结构性调整

目前没有场景证明必须增加新的身份类型。唯一值得保留的条件性候选是：
驱动必须在caller Direct中动态扩充provider长期DMA池，无法提前在B Init/Worker分配，
又有测量证明通过已有Gate/Worker移交不满足负载；此时device-agnostic alloc不能表达所需归属。
可评估**一个以已claim DeviceId为依据的device-related DMA分配入口**，与原alloc分别命名，
Core从device表继承owner并复验lifecycle/域/backing，不接受任意ComponentId当授权。
这不是本轮实施项，也不是为统一而自动scope Direct。

| 项目 | 条件性方案C |
|---|---|
| 必须涉及文件 | `abi/core.toml`、生成出口/SDK-C/Rust ABI、`component/export.rs`、`resource/dma.rs`/`device.rs`、driver/memory/deployment契约、VirtIO或真实目标Hal及测试 |
| Core ABI | 是，新增一个窄操作并协调fingerprint；不加版本后缀/旧alias；无需求证据不修改 |
| 新运行时状态 | 可复用Allocation.owner/device表；若未来需要backing pin，必须明确pin关系，不仅多记一个OwnerId |
| Direct性能 | 仍不加每次method的Core边界；动态分配操作本来已进Core，多device/lifecycle复验；需基准证据 |
| Gate | 可用同对象规则，不改变等待限制；原device-agnostic alloc保留临时caller语义 |
| SMP | device owner解析、grant commit与failure串行；不得resource锁内反查registry造成反序 |
| Sandbox | 同名业务语义不能保证同transport；U必须有已验证device访问与backing来源，不能直接借DeviceId授权 |
| 新不变量 | device-related backing随合法device owner生命周期；caller借入backing与此分配明确不同 |
| 可删特例 | 真实Hal无需靠“必须在Init分配”规避长期动态分配；不删除普通ambient alloc或Native unmap规则 |
| 最大风险 | 没有负载就添ABI；把device ownership当U授权；错误推定映射有owner就保证backing驻留 |

若新需求其实是跨owner通知或共享AS部署，应分别提交具体场景和最小设计，不借C扩成Capability/IPC框架。

## 10. 尚未解决的问题与下一轮实施清单

尚未解决：B1–B4没有本轮硬件/固定交错复现；S1–S3需要明确合法执行契约；
任何backing pin、跨owner通知、真实U组件server、完整回收都不在本轮实现范围。
是否删除creating fallback、是否改变K/K Gate-only bind、是否需要C，应以调用点审计和真实需求为决策依据。

下一轮由人类实现，Agent可以补相应host/单元/CoreTest测试与审计记录：

| 顺序 | 可执行工作 | 验收条件/验证层次 |
|---|---|---|
| 1 | 固化B1–B4最小复现；先补测试组件、test-only同步手段，避免改生产逻辑来强行制造结论 | host固定交错证明grant/撤销与controller顺序；QEMU证明B4真实stack/identity和B2真实trap；标明预期旧失败 |
| 2 | 写出registry→CPU/Task或device→IRQ/DMA等实际锁序，列所有反向查询 | 无锁跨组件调用；同CPUtrap不能重入持有锁；SMP不能新grant到Failed owner |
| 3 | 人类修grant提交点；覆盖caller!=device owner的map及UserDomain修改；处理未发布lease回滚 | 错owner、Failed owner、过期id、失败路径；Failed后无live新grant；UserDomain补rv64 S/MMU验证；保留合法teardown |
| 4 | 人类修IRQ irq-save与controller串行提交；澄清release与callback drain | host验证锁/表顺序，QEMU双CPU真实投递；已准入callback仍计inflight阻止Stop |
| 5 | 人类修Init/Exit下yield/park/exit许可；保留anchor sched_run与Init Worker创建 | QEMU sentinel归属及panic归因；合法Task/Direct调度通过，Gate/IRQ/Policy错误路径拒绝 |
| 6 | 人类修mapping id耗尽；明确create/load caller/context门禁 | host near-max不回绕；Failed/IRQ/Policy调用错误路径；合法嵌套Init路径 |
| 7 | 采用B的资源与等待规则，明确Block/Gate合法上下文、Gate-only bind策略 | ABI/SDK/权威文档一致；不宣称I真实VirtIO/Userver已支持 |
| 8 | 审计creating全部调用点，再决定显式owner内传/去fallback；修M1–M3 | host与Arch/QEMU Init/Exit/Gate/IRQ/task provenance恢复；无额外身份registry |
| 9 | 运行与变更对应的host、CoreTest、ArchTest；变化触及SMP/I则加入相应配置 | 报告协议逻辑与硬件生效两层证据；复核第一轮已修事务，避免回退 |
| 10 | 只有真实负载提出长期动态DMA/跨owner通知需求时才评估C或更窄原语 | 先给调用链、性能/生命周期证据和现有模型表达缺口；再改ABI，不追加通用框架 |

同一事实避免重复测试：host验证表/状态/错误提案，CoreTest经真实公开API编排，
寄存器保存、IRQ/controller、AS/PTE/TLB真实效果由ArchTest/QEMU/真机验证。
业务queue和借用期限由组件测试，不用CoreTest god-mode修改Core私有状态。

## 11. 原审计阶段验证记录

实际执行：

```sh
CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=/usr/bin/cc \
  cargo test -p kernel --lib -- --test-threads=1
```

结果：**506 passed，0 failed，5 ignored**。本轮日志：`/tmp/kaleidos-execution-audit-host.log`（会话临时文件）。
5个ignored均是`make bench`手动执行的性能基线；不是失败路径测试被跳过。
编译出现已有的test-support dead-code警告（Rank::Load、align4_test），无测试失败。

相关通过项包括：nested Init/IRQ/Service identity恢复、caller task provenance、destroy identity、
Direct publication保活、Gate/IRQ inflight阻止Stop、DMA quarantine、同owner unpark/park表提交。
但host假后端不执行真实组件入口，且本轮没有新增固定交错用例，
因此**不能**从506个通过项推出B1–B4不存在或已硬件复现。

本轮没有重跑QEMU、ArchTest、全workspace带fixture测试或benchmark。
[第一轮Core convergence](core-convergence.md) 中的既往QEMU记录是历史证据，不能算本轮结果。
文档交付另检查ABI矩阵61项无遗漏/重复、仓库内源码链接存在、行号在范围内和diff空白错误。
审计与方案评估到此结束；实现和契约变更留给下一轮。


## 12. 授权后的修复与验证（2026-10-08）

用户授权修复后，B1–B6 已完成生产实现、回归测试及对应契约更新。沿用现有
ComponentId、TaskId、AS 与资源表，Core ABI 的类型、符号和 fingerprint 不变。
§1–§11 是历史基线，下面链接指向当前工作区实现。

| 问题 | 修复 | 新回归证据 |
|---|---|---|
| B1 | claim、IRQ register / enable、DMA alloc / map 持 registry 锁复验 Starting / Ready 至登记结束；map 检查 caller 与设备 owner。UserDomain 的 AS 创建、Task 关联及 map / copy / prepare / protect / clone / replace 也在相应提交事务复验；Task 关联前失败则移除尚未发布的新 Task | host 覆盖失败 caller、失败设备 owner、合法不同 owner 的 Native map；未发布 lease 拒绝后回收；64 轮双线程 alloc / failure 竞争结束后无迟到 allocation |
| B2 | IRQ 操作取得 device / IRQ 表锁前屏蔽本地 IRQ，释放锁后恢复 | host controller hook 检查 enable / disable / release 提交点的 IRQ 状态和锁持有；QEMU IRQ 回归通过 |
| B3 | enable / disable / release 保持 device / IRQ 锁至有界 arch 控制器写入完成 | host 在旧 release 的硬件提交点暂停线程，新 register / enable 必须等其完成，最终新 route 与 enabled 状态一致 |
| B4 | Init / Exit 祖先链禁止 yield / park / exit；保留 Init 创建、启动 Worker 与 anchor sched_run | host 检查错误码及 principal 恢复；RV64 / RV32 CoreTest LifecycleProbe 在真实 A Task → B.create / B.destroy 栈上调用公开 ABI，三种操作均 EINVAL，B 发布与退出成功 |
| B5 | mapping id checked_add，耗尽返回 EOVERFLOW，不改变 mapping 表或复用旧 id | host 从 u64::MAX−1 开始，撤销最后成功 id 后重复 map 均拒绝，旧 id 仍无效 |
| B6 | create / load 拒绝失败 caller、IRQ 和 Policy 上下文；装载后在 registry 的新实例登记事务再次复验 caller | host 覆盖公开 create / load 拒绝、out_id 不变与已准备镜像的最终登记拒绝；QEMU 合法嵌套 create / stop 回归通过 |

实现入口：[device](../../os/core/src/resource/device.rs)、[DMA](../../os/core/src/resource/dma.rs)、
[IRQ](../../os/core/src/resource/irq.rs)、[UserDomain](../../os/core/src/task/user.rs)、
[containment](../../os/core/src/component/containment.rs)、[scheduler](../../os/core/src/sched.rs)、
[load](../../os/core/src/component/load.rs)、[isolated create](../../os/core/src/component/isolated_lifecycle.rs)。
真实栈用例复用 [kcomp_checksum](../../os/components/tests/kcomp_checksum/src/lib.rs)，
由 [CoreTest convergence](../../os/components/tests/core_test/src/runtime/convergence.rs) 仅经公开 API 编排。

M1–M3 已同步：组件模块页改为 61 项 ABI 和现有 Isolated 支持面；测试指南修正 stop ABI；
DMA map 注释说明 device owner 与 caller 可不同；IRQ route / release 注释明确已经准入的
callback 可以完成，release 不承诺 drain。生命周期、驱动和调度契约已记录此次门禁与提交规则。

验证层次与结果：

| 命令 | 结果 | 证据范围 |
|---|---|---|
| `cargo test -p kernel --lib -- --test-threads=1`（host linker=/usr/bin/cc） | 514 passed，0 failed，5 ignored | 不含装载工件的 Core host 逻辑测试；ignored 为性能基线 |
| `make test-host`（同 host linker） | 通过；含真实 fixture 的 Core 561 passed，0 failed，6 ignored，其他 workspace / SDK / 工具测试通过 | 包括 loader 工件和默认并行 host 回归；不证明硬件隔离 |
| `make _test-qemu-rv64` | default / no-block 各 80 checks PASS | 真实组件生命周期栈、调度、资源公开 API 与 RV64 S/MMU 用户执行回归 |
| `make _test-qemu-rv32` | default / no-block 各 58 checks PASS | RV32 真实栈与资源公开 API 回归 |
| `make test-arch` | RV64 43/43、RV32 43/43、RV64 SMP 3/3 PASS | 既有 trap / IRQ / AS / 跨 CPU 硬件契约回归 |
| `make fmt-check` | 通过 | Make 库存中的 Rust 格式 |
| `make clippy` | 通过，`-D warnings` | host Core / Arch / SDK 与 Make 库存的 RV64 组件 |
| RV64 Core `cargo clippy --no-deps`（使用 genmk 的 resolved 值，`-D warnings`） | 通过 | 真实 UserDomain 编译分支；不扩大到依赖的 lint |
| `make abi-check`（随 `make test-host`）与 `git diff --check` | 通过 | ABI 生成物未漂移、diff 无空白错误 |

会话日志：`/tmp/kaleidos-ownership-host.log`、`/tmp/kaleidos-ownership-host-full.log`、
`/tmp/kaleidos-ownership-qemu-rv64-complete.log`、`/tmp/kaleidos-ownership-final-rv32-arch.log`、
`/tmp/kaleidos-ownership-clippy-complete.log`、`/tmp/kaleidos-ownership-core-target-clippy.log`。QEMU 完整日志保存在 `build/tests/*/logs/`。
部分首次构建在 kcomp-link 的 ET_REL 校验处中止，按相同命令重跑后通过；没有修改
构建工具，也没有把重跑成功视为该偶发构建问题的修复。

B1 的双线程测试覆盖竞争结果，并未固定失败恰好发生在 allocation 的哪条指令；
B3 固定交错由 host fake controller 提供，不是双 CPU 真实 PLIC 竞态复现。
B2 的 host hook 证明临界区的 irq-save 纪律，既有 QEMU IRQ 场景提供硬件回归，
没有新增在持锁处强制 IRQ pending 的 ArchTest。UserDomain 的真实路径由 RV64 S/MMU
场景回归；没有新增该路径的固定 failure 交错。B4 的新增场景验证真实栈门禁、发布归属和
正常销毁，未新增 lifecycle panic 变体。上述界限不能由总通过数替代。

S1–S3 的其余语义设计仍需单独决定；其中 IRQ create / load 的耗时路径已随 B6 禁止。
Gate-only bind、Block 等待规则、Policy 的其他分配入口、creating fallback 收窄均未借本次
修补扩展。L1–L5 是既定限制；F1–F3 是未实现能力，DMA backing pin、设备静默、U-mode
组件服务与完整回收不能从这些修复推导为已支持。
