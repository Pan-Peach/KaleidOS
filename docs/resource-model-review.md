# Core 资源模型 / Authority 模型审查

> 对象：`os/core/` 的数据结构、资源模型、鉴权模型、生命周期模型。
> 依据：5 份代码盘点（handle/slot 表、鉴权与调用上下文、ResourceDomain 与 teardown、docs-vs-code、interface lease 与生命周期）+ Oracle 独立评审。
> 基线：commit `c833863`（image/instance 拆分，`docs/component-lifecycle.md` 为已冻结的组件生命周期契约）。
> 结论：**提案的方向大体成立，但它的目标多半已经是现状；真正要做的三件事与提案的排序不同。**

---

## 0. 对提案的裁定（先看这个）

| 提案 | 裁定 | 依据 |
|---|---|---|
| §11「ResourceDomain 重复 truth，应降级」 | **已经是现状**：`ResourceDomain` 不是类型。文档写明"实现决策：第一版没有 struct"，并给了 `// ✗ 不要这样` 的反例 | `docs/component-model.md:128-180`、`handle/mod.rs:18`；代码零命中 |
| §11「owner 存两份」 | **不成立**：所有权只存一处 `Slot.owner`。任务/地址空间各有自己的 owner——那是**不同对象**的 owner，不是重复记账 | `handle/generic.rs:78-82`、`task/record.rs:14`、`memory/address_space.rs:80` |
| §12「Scheduler 不该拥有 TaskHandle / FrameHandle」 | **已满足，应表述为"保持"**：`TaskHandle`/`FrameHandle` 在代码里**不存在**；调度器已只收候选 ID 并提议，由 Core 验证 | `handle/mod.rs:17-18`、`sched.rs:152-182` |
| §3「每个 Authority 必须是 XxxHandle 是错的」 | **需要修正的是文档措辞，不是结构**。handle 本身不携带 owner、也不是 authority，但**移除它不会带来安全收益**：typed ID 可以保住 generation/类型/O(1)。真正的错误是文档仍称 handle"不可伪造" | `handle/generic.rs:10-17,38-48`、`docs/core-philosophy.md:152-175` |
| §10「Slot 泄漏成架构概念」 | **已经是现状**：`Slot` 是 `pub(crate)`，组件只见不透明 `u64` | `handle/generic.rs:78`、`handle/mod.rs:47-48` |
| §4「CallContext 缺失」 | **方向对，但「没有 principal 机制」是错的**。create/destroy/task 执行**都**有 Core 建立的身份；缺的是**普通直调**与**IRQ 回调**的归属 | 见 §C.1 |
| §8「需要 Grant」 | **净新增**，代码里完全没有跨 owner 授权 | `handle/table.rs:29-48`、`driver-model.md:403` |
| §9「Lease 职责混杂」 | **对，且比提案说的更糟**：三个同名类型里有两个什么都不 pin | 见 §C.3 |
| §13「DMA allocation ≠ mapping」 | **对**，但拆分必须保留"设备权威证明"，不能只是删掉设备字段 | 见 §C.4 |
| §18「不要为未来发明 authority object」 | **同意**，且应据此**拒绝** `ExecutionDomainId` 注册表、通用 capability 图、`ObjectMeta` 大迁移 | 见 §G 拒绝清单 |

**一句话**：提案把"已经做对的"当成了"要做错的"，把"真正的缺口"（调用归属）排在了"想做的简化"（Handle→ID）后面。**正确顺序是：先补归属，再动关系，最后才谈命名。**

---

## A. 当前模型盘点

| 名称 | 文件 | 当前职责 | 已实现 | 重复 truth | 潜在问题 |
|---|---|---|---|---|---|
| `Handle<T>` | `handle/generic.rs:14` | Identity + 索引；**不携带 owner** | ✅ | 否 | 文档称其"不可伪造"（不实）；generation 会 **wrap**（`generic.rs:123`） |
| `Slot<T>` | `handle/generic.rs:78` | Ownership + Liveness + 索引 | ✅ | **否（唯一 owner）** | 概念上承担 4 个角色，但 `pub(crate)`，未泄漏 |
| `ResourceTable<T>` | `handle/table.rs:16` | Vec 索引表、revoke | ✅ | 否 | `release` 在 generation/owner 通过后**不检查** object liveness |
| `MmioRegion` / `Irq` / `DmaRegion` | `handle/{mmio,irq,dma}.rs` | 资源 payload | ✅ | 见 §C.4 | 各自独立存 `device_index`——**是三个对象与设备的关系，不是三份 owner** |
| `MmioLease` / `DmaLease` | `handle/lease.rs:26,65` | **只读指针/provenance 快照**（`Copy`） | ✅ | provenance 重存 `(slot, generation)` | **不是 lease**：无 `Drop`、不 pin、撤销协作式 |
| `MemoryLease` | `memory/mod.rs:62` | **唯一 RAII 分配属主**（区域占用） | ✅ | `region.size == 1 << order` | 名字叫 lease，语义是"独占分配所有权" |
| `RequestContext` | `handle/context.rs:26` | 环境 principal | ✅ | — | 无 `execution_domain`；单全局栈；普通直调不切归属 |
| `ComponentId` | `component/mod.rs:40` | **实例身份** | ✅ | — | 保持；**不要**再加平行 `ComponentInstanceId` |
| `ComponentImageId` | `component/image.rs` | 镜像身份 + `MemoryLease` | ✅ | — | pinned-until-reboot，无 refcount（刻意） |
| `TaskId` / `TaskRecord.owner` | `task/id.rs:7` / `task/record.rs:14` | 任务身份 / 任务 owner | ✅ | — | `TaskId` 无 generation（单调，wrap 后才可能 ABA） |
| `BindingRecord` | `component/interface.rs:191` | 接口 provider 槽 | ✅ | — | **无 consumer 边**；`generation` 是发布计数器，**不是存活 epoch** |
| `AddressSpaceHandle` / `KernelAddressSpace` | `memory/address_space.rs:26,77` | 第二套平行 handle 设计 | ⚠**休眠** | — | `owner` 存了**从不检查**；`generation` 恒为 1；manager **生产路径从未实例化**、ABI 未导出 |
| `ResourceDomain` | — | 文档视图 | ❌不存在 | — | 无 |
| `TaskHandle` / `TimerHandle` / `FrameHandle` | — | 文档枚举 | ❌不存在 | — | 文档承诺了代码没有的东西 |
| `Slot` 泄漏 | — | — | — | — | `pub(crate)`，未泄漏 |

---

## B. 概念分类

| 概念 | Identity | Object | Ownership | Relation | Grant | Lease/liveness | Exec Context | Derived | Impl |
|---|---|---|---|---|---|---|---|---|---|
| `Handle<T>` | ✓ | | | | | (gen 防 stale) | | | ✓ 索引 |
| `Slot<T>` | | | ✓ | | | ✓ | | | ✓ |
| `MmioRegion`/`Irq`/`DmaRegion` | | ✓ | (在 slot) | ✓ 与设备的关系 | | | | | |
| `Irq.delivery` | | | | ✓ IRQ→handler | ✓ 回调投递 | | | | |
| `DmaRegion.lease` | | | | ✓ buffer↔device | | ✓ `MemoryLease` | | | |
| `MmioLease`/`DmaLease` | ✓来源 | | | | (快照) | ✗ **什么都不 pin** | | | |
| `MemoryLease` | | | ✓ 分配属主 | | | ✓ RAII | | | |
| `RequestContext` | ✓ | | | | | | ✓ | | |
| `BindingRecord` | ✓ | | (provider 归属) | ✓ provider 槽 | | ✗ | | | |
| `ComponentId` | ✓ | | | | | | | | |
| `ComponentImageId` | ✓ | | ✓ `MemoryLease` | | | | | | |
| `TaskRecord.owner` | | | ✓ | | | | | | |
| `AddressSpaceHandle` | ✓ | | ⚠存了不查 | | | ⚠恒 1 | | | |
| `quarantine[256]`(MMIO) | | | ✗ | | | | | | ✓ 设备可用性闩 |
| DMA `QUARANTINE` | | | ✗ | | | ✓ backing 停车 | | | ✓ |
| `owned_by` 索引 | | | | | | | | ❌不存在（O(N) 扫描，刻意） |

---

## C. 职责混杂点（真正的发现）

### C.1 调用归属：缺的是两条路径，不是"整机制"（**本条修正了我的初稿**）

现状：create / destroy / task 执行**都有** Core 建立并恢复的归属（`containment.rs:299-336,376-390`）。**IRQ 回调这一半已由 §G step 3 关闭**（**不改任何 ABI**：无签名 / 布局 / 导出变更）；仍缺的是：

- **普通直调**：A 调 B 的函数表，不安装任何边界 → `ambient()` 在 B 内解析为 **A**。这是**已冻结契约的有意选择**（`docs/component-lifecycle.md:182-191`：provider 应在自己的生命周期/任务上下文里获取资源）。
- **IRQ 回调（已落地，step 3）**：`RouteOutcome::Callback` 的 `owner`（`irq/mod.rs:121-133`）现在经 `with_irq_scope` 安装归属 → 回调内 `ambient()` 解析为该线 owner、`task = None`，被中断的边界在回调返回后恢复；作用域内调度类调用返回 `-EINVAL`，回调 panic 保持致命。
- **调度器策略回调**仍没有 provider scope（`sched.rs:177`）——盘点漏了这条，是 **step 4 的职责**。

同时必须承认：`ambient()` 读的是 `active_escape()`（`handle/context.rs:40-46`），所以**归属与 panic 路由是耦合的**。IRQ 作用域因此**没有**可恢复的 panic 语义（回调 panic 致命）；为其它边界加 principal 字段也不能自动获得可恢复的 provider panic。

### C.2 真正"过度"的是 `AddressSpaceHandle`/`KernelAddressSpace`（但**不要删整个模块**）

- `get`/`get_mut` **不接受 caller**，`owner` 从不检查，`generation` 恒为 1（`address_space.rs:210-247`）；
- manager **生产路径从未实例化**，`kcore_address_space_map` 刻意**不在**导出白名单（`export.rs:1289` 有断言）。
- 但 `KernelAddressSpace` 本身有价值：范围校验、backend 失败/提交顺序、不同粒度的契约测试（`:131-196,342-380,701-739`），且 `memory/mod.rs:15` 重导出它的 `PhysicalRange`。
- **裁定**：本轮**保持休眠**；将来若做定点缩减，只移除那层未使用的 owner/generation 门面，保留 mapping/backend 与其测试。

### C.3 "Lease" 是三个不同东西（**比提案说的更糟**）

| 类型 | 真实语义 | 建议 |
|---|---|---|
| `MemoryLease` | **独占 RAII 分配属主**（区域占用） | 语义正确，名字偏弱；可留 |
| `MmioLease` / `DmaLease` | `Copy` 的指针+provenance **快照**，无 `Drop`、不 pin | 内部改名为 **`MmioView` / `DmaView`**（不要用泛化的 "Capability"）。**导出名 `_lease` 的改动 = ABI 改动** |

interface binding **根本不是 lease**：无 consumer 边、无 refcount、`generation` 只在替换时递增；`BindingView` 是 `Copy`，SDK 直接把缓存指针给消费者（`binding.rs:102-120`）。今天不出事靠的是 **image/state 常驻**，不是引用计数。

### C.4 DMA：拆分**必须保留**"设备权威证明"

三个 `device_index` 各自标识对象与设备的关系，不是三份 owner。当前的合并操作**刻意**同时完成"证明设备权威"与"与 root release 串行化"（`dma.rs:207-248`）。拆成 alloc/map 后，这个证明必须落到 **map**，而不是被删掉。

真正的重复很小：`DmaRegion.device_addr` == `MemoryLease.region.base`、`DmaRegion.size` == `MemoryLease.size`（v1 恒等）；`region.size == 1 << order`；`lease.source` 重存 `(slot, generation)`。

**一个必须写进契约的区分**：**从未暴露给设备的 backing** 可以正常释放；**可能仍被 DMA 访问的 backing** 必须停车（今天的 `QUARANTINE`）。若拆掉设备字段后直接让 `MemoryLease` drop，就把今天的安全措施抵消掉了。

### C.5 其它被盘点/提案漏掉的

- **generation 会 wrap**（`generic.rs:123`）→ 过期排除不是永久的；最小严格修法是**耗尽即退役该 slot**。
- **ABI 是不带 tag 的 `u64`**，资源种类未编码（`ResourceKind` 只用于 trace）→ 裸 MMIO token 若误传给 IRQ 操作，类型系统拦不住。
- **失败路径并非都走共享兜底**：非法调度提议直接 `mark_failed(provider)`，**绕过** `fail_component` 的资源清理（`sched.rs:176-182` vs `failure.rs:46-49`）。
- **failure 的 IRQ revoke 只摘软件投递，不关硬件线**；显式 `irq_release` 才关（`irq.rs:254-255` vs `:369-378`）。
- `deny_if_failed` 只排除 `Failed`，不排除其它"不应获得新权威"的状态。
- **引导期缺口**：`kcore_component_create` 在 B 的 create **返回之后**才给出 B 的 ID（`load.rs:100-119,121-144`）→ "先 grant 给 B、B 在自己的 create 里用"**不自动成立**，需要一个 attach 服务或显式 pregrant 契约。
- SMP 需要的是 **CPU-local 的"当前执行/escape 状态"**，**不是**把每张身份表按 CPU 复制。

---

## D. 提议的最小 Core Data Model

### 保留（不动）

```
Handle<T>  /  Slot<T>  /  ResourceTable<T>  /  按表 owner  /  共享 teardown 兜底
ComponentId(=实例)  +  ComponentImageId(=镜像)
```

### 新增（最小）

```text
# 1) 窄的 provider 调用作用域（不是通用 set_principal）
ProviderCallScope:
    由 SDK 提交 binding id + 期望 generation
    Core 校验 provider 仍 Ready 且快照未被替换
    建立 provider 归属；调用后恢复前一个 scope
    component = provider, task = 物理任务（若有）
    不得赋予 create 期发布权（ambient_init 仍只认真正的 create 边界）
    同步、不可 yield、不可重入越界；panic 在此 scope 内**致命**

# 2) IRQ 归属作用域
    由 RouteOutcome::Callback.owner 建立，task = None
    单独保存/恢复被中断的上下文
    不改变回调签名

# 3) MMIO Grant（typed，ACL 式，非 bearer token）
MmioGrantRecord:
    grantor: ComponentId          # 签发者；object 的 owner 仍是 Slot.owner
    grantee: ComponentId
    object:  MmioHandle           # 含资源 incarnation
    rights:  MmioRights           # 只含真实需要的：读写 / 派生 native view / (必要时) IRQ、DMA use
    state:   占用即有效
授权判定： owner == principal  OR  存在匹配 (incarnation, grantee, right) 的 live grant
```

**不要建**：delegation chain、grant-of-grant、权限放大、badge、capability 树、通用 object-union、任何新的 domain identity。
**不要用** `ExecutionDomainId` 注册表：phase 1 只有 KernelNative，且生命周期契约明确排除通用 domain manager。

### 关系显式化（仅在 Grant 出现后才必要）

Grant 会**打破**"设备 owner == IRQ owner == DMA owner"这个等式，于是"撤销 A 时哪些子对象必须停"就不能再靠 owner 扫描回答。此时才需要显式记录 root/grant 依赖（子 IRQ/DMA 记录保留 root incarnation 与授权 grant 身份）。**在此之前不要建 consumer 依赖图**——契约已明确推迟 callback draining 与 refcount 驱动的 unload。

---

## E. 鉴权流程

| 场景 | 流程 |
|---|---|
| **owner 自访问** | `ambient()` → principal；`ResourceTable::get` 查 slot→generation→`owner == principal`→liveness；执行 |
| **跨 owner Grant** | 同上，但 owner 不等时**再查 grant 表**（incarnation + grantee + right）；命中则执行。**不做内部冒充 A**（不要用 `get(A, handle)` 复用） |
| **Scheduler 提议** | 调度器（provider scope 内）调 `choose_next` → 提交候选 `TaskId` → Core 验证存在/可运行/未在别处运行 → commit。调度器**不持有**任何任务 handle |
| **Allocator 提议** | phase 1 无 allocator 组件；帧分配是 Core 内部机制。未来 `MemoryPolicy` 只能提偏好 |
| **isolated 组件 syscall** | 未来：trap 上下文产生 principal；今天只存在于文档。**资源模型不变**是要求 |
| **Wasm host call** | 同上，runtime instance 产生 principal；更远 |
| **native 直调** | 今天：不切归属（有意）。需要 provider 归属的服务 → 走 §D.1 的窄 scope；否则 provider 在自己的 create/任务上下文里取资源 |
| **IRQ 回调** | 由 `Callback.owner` 建立归属（task=None）；**IRQ 安全子集**：不得阻塞/分配/任意取锁 |

**诚实边界**：协作式 enter/leave **无法证明**真正执行的是被声称的 vtable 函数——它是**可信组件下的正确记账**，不是抗冒充边界。不要把它宣传成"代码认证"。

---

## F. 生命周期流程

| 阶段 | 流程与不变量 |
|---|---|
| **component create** | 声明镜像/实例 → create 边界安装归属 → `kcomp_instance_create(args, &out_state)` → 记录 state → 提交 pending 发布 → Ready |
| **resource acquire** | 在**自己的** create/任务上下文里 claim；device 身份从已持有的 MMIO root 派生；`deny_if_failed` 只挡 `Failed`（**已知不足**） |
| **relation / bind** | 接口 bind 是**无状态查询**，不建 consumer 边，不 pin provider |
| **cross-owner grant** | 仅当 owner 之外要用；由 owner 签发；root/子对象失败时必须级联撤销（含**属于 B 的**、从 A 派生的 IRQ/DMA） |
| **interface lease** | 今天**不存在** lease 语义；image/state 常驻是唯一保护 |
| **graceful stop** | 拒绝存活任务 → Stopping → `kcomp_instance_destroy`（在被停实例身份下）→ 兜底 revoke/unbind → Stopped |
| **forced containment** | 直接 `Failed`（不调 destroy）→ MMIO quarantine 标记 → IRQ revoke（**只摘软件投递**）→ DMA backing 停车 → unbind + discard pending |
| **resource reclaim** | **不做物理回收**：image pinned-until-reboot、Stopped/Failed 留 tombstone、`ComponentId` 不复用 |

**顺序不变量**：DMA 必须**先**把 `MemoryLease` 停进 `QUARANTINE` 再 revoke（否则 Drop 会在设备可能仍 DMA 时释放 backing = UAF）；MMIO 必须**先**标记 device quarantine 再 revoke。**把 teardown 换成通用的"按 owner 扫描"会丢掉这些生命周期动作**——这正是 §11 那条简化建议的风险所在（但原因是"不能以无差别销毁替换生命周期动作"，而不是"不能按 owner 扫描"）。

---

## G. 渐进迁移计划

**排序原则**：**调用归属先于 DMA 拆分与 Grant 暴露**。否则新的所有权记录仍可能被记到错误实例上，把歧义放大而不是消除。

每一步都是**可绿的落地单元**（实现与其测试同批提交）；TDD 期间可局部红，但**不提交破损迁移**。

| # | 内容 | ABI |
|---|---|---|
| 1 | **修正本审查 + 冻结范围**：保留 `ComponentId`、generational handle、按表 owner、共享 teardown；显式批准任何对 `component-lifecycle.md` §7 与 deferred-Grant 规则的偏离 | 无 |
| 2 | **锁住既有不变量 + 修正误导性词汇**：stale 复用、teardown/quarantine、create/destroy 恢复；内部指针快照按需改名（保留导出名） | 无 |
| 3 | **IRQ 归属 + 上下文种类限制**（内部）：owner 选择/恢复、禁止在 IRQ 上下文做调度操作；QEMU 验证真实中断进出（**已落地**；归属作用域目前由 host 测试锁定，QEMU 门尚未触发 Core 路由的 IRQ 回调） | 无签名/布局变更，但需记录行为契约 |
| 4 | **窄 provider 调用 scope + 迁移需要它的服务**：嵌套、stale binding 拒绝、发布限制、不可 yield、panic 策略同批落地 | **协调替换**（新 `kcore_*` 边界入口 + SDK/C 声明） |
| 5 | **DMA backing 与 device mapping 拆分**（若用例仍需要）：allocation 归实例；mapping 同时校验 buffer 访问与 MMIO 派生的设备权威，保留 backing 生命周期与暴露后的 quarantine | **协调替换**（若 `kcore_dma_alloc` 去掉 MMIO 参数或 `_lease` 语义/输出变化） |
| 6 | **MMIO Grant + 真实交接工作流 + 依赖清理同批**：attach 引导、owner/grantee 失败、子对象授权、root-release 检查、grant/resource generation 测试 | **协调替换** |
| 7 | **停在休眠域工作之前**：跑完 host/构建/打包 + RV32/RV64 QEMU/ArchTest 门；地址空间激活、物理 unload、通用 delegation 继续推迟 | 无 |

**每个改动 ABI 的单元**：`kcomp.h` + Rust 声明 + Core 导出 + 受影响指纹 + 组件**一起**改；拒绝陈旧产物、重建包（`kcomp.h` 已确立该契约，验证门见 `docs/component-lifecycle.md` 末节）。

### 明确拒绝

用改名 ID 取代 handle（不带来安全收益）、`ResourceDomain` 容器、通用 `ObjectMeta` 大迁移、调用方自报 principal 当作"认证"、`ExecutionDomainId` 脚手架、通用 capability 图、绕过 quarantine 的 owner-only 销毁、仅凭接口/实例计数做物理回收。

**工作量**：审查/文档修正 **Short（1–4h）**；正确的调用 scope + IRQ/panic 测试 **Medium–Large（1–3d+）**；完整 DMA 拆分 + prober→driver Grant 生命周期 **Large（3d+）**，不是廉价清理。
