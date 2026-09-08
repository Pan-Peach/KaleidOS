# 组件模型（component-model.md）

## 1. Component 是什么

Component 是 KaleidOS 的基本构建单元。它不只是"一个模块"，而是一个完整的可管理单元：

- 消费（requires）和提供（provides）Interface；
- 拥有 ResourceDomain（Core 维护的资源集合）；
- 有生命周期（见 §5）；
- 可以依赖其他 Component；
- 可以包含子 Component（Composite，见 §7）；
- 可以被替换 / 重启 / 恢复。

**组件 ≠ crate**：第一阶段里，一个只有几十行的小模块就是普通 Rust module，不需要为架构图强行建 crate。组件是概念边界，crate 是实现选择。

## 2. Interface：Device / Service / Policy

Interface 表达"这个组件提供什么能力"，按领域分三类：

| 类别 | 含义 | 例子 |
|---|---|---|
| Device | 访问硬件的抽象 | BlockDevice、NetDevice、InputDevice、DisplayDevice、AudioDevice |
| Service | 跨组件的系统服务 | FileSystemService、NetworkService、GraphicsService、LoggerService、GameRuntimeService |
| Policy | 可替换的策略算法 | SchedulerPolicy、PageReplacementPolicy（未来：MemoryPolicy） |

### 例子：驱动组件图

```text
NVMe Component
requires:  MmioHandle, IrqHandle, DmaHandle   （Authority，来自 Core）
provides:  BlockDevice                        （Interface）

Ext4 Component
requires:  BlockDevice, PageCache
provides:  FileSystem

VFS Component
requires:  FileSystem providers, PageCache
provides:  FileSystemService
```

> Interface 是语义，传输是绑定策略。第一阶段用 Rust trait + direct call；
> 未来可换 IPC stub / Wasm host call。接口文档里写"契约"（方法、语义、错误），不写"怎么调用"。

## 3. ResourceDomain —— 一个"视图"，不是一个对象

**实现决策：ResourceDomain 第一版没有 struct。**

```text
ResourceDomain(ComponentId(7))
=
Core 里所有 owner == ComponentId(7) 的 authority 资源
```

不写外置集合：

```rust
// ✗ 不要这样
struct ResourceDomain {
    irq_handles: Vec<IrqHandle>,
    mmio_handles: Vec<MmioHandle>,
    dma_handles: Vec<DmaHandle>,
}
```

而是资源自己的表记录 owner（数据库"视图"的直觉）：

```rust
struct IrqRecord {
    owner: ComponentId,
    irq: IrqId,
    generation: u32,
}

struct MmioRecord {
    owner: ComponentId,
    range: PhysRange,
    generation: u32,
}

struct DmaRecord {
    owner: ComponentId,
    // ...
    generation: u32,
}
```

> **KernelNative 的 Core 与组件共享一个 Core heap**：ResourceDomain **不**追踪 per-component 的堆分配或字节计费，也没有 per-component arena / 私有堆。它只记录 authority handle（MMIO/IRQ/DMA/frame handle），用于保护与 revoke。
>
> `ComponentId` 是 identity（不是 authority），`handle.rs` 把 Handle 定义成 Core 创建、类型化的 authority —— 两者已经明确分离。

### 3.1 Handle table 可以非常普通

```rust
struct Slot<T> {
    generation: u32,
    owner: ComponentId,
    object: T,
}

pub struct Handle<T> {
    slot: u32,
    generation: u32,
    _marker: PhantomData<T>,
}
```

```rust
type IrqHandle = Handle<Irq>;
type MmioHandle = Handle<MmioRegion>;
type DmaHandle = Handle<DmaMapping>;
```

control path（Core 校验，必须记录 trace）：

```rust
fn get_irq(caller: ComponentId, handle: IrqHandle) -> Result<&Irq, HandleError> {
    let slot = IRQ_TABLE.get(handle.slot)?;
    if slot.generation != handle.generation {
        return Err(HandleError::Stale);
    }
    if slot.owner != caller {
        return Err(HandleError::WrongOwner);
    }
    Ok(&slot.object)
}
```

### 3.2 回收：revoke_owner

```rust
fn revoke_component_resources(id: ComponentId) {
    irq::revoke_owner(id);
    mmio::revoke_owner(id);
    dma::revoke_owner(id);
    timer::revoke_owner(id);
}
```

每张表内部：

```rust
fn revoke_owner(owner: ComponentId) {
    for slot in TABLE.iter_mut() {
        if slot.owner == owner {
            revoke(slot);
        }
    }
}
```

> 第一反应可能是"扫表性能是不是不好？"——但这条路径是 **component unload / failure / restart，不是 fast path**，O(全部 IRQ + MMIO + DMA handle) 完全可接受。第一版不做 `ComponentId -> Vec<ResourceRef>` 反向索引；等真发现资源量大再维护。

### 3.3 组件停止时的回收 —— 两条路径，不预设 universal revoke order

> Core 的保证是 **eventual revocation / containment**：组件生命周期结束后，Core 最终必须收回其 ResourceDomain。
> 具体顺序**不写死** —— 不同设备要求不同：有的要先停 DMA、reset 设备再 mask IRQ；有的要先 unmap。
> 落地原语统一收敛为上面的 `revoke_component_resources(id)`。

#### Graceful shutdown（正常关闭）

```text
quiesce                        —— 停止接受新请求
  ↓
component-specific shutdown    —— 设备相关收尾（停 DMA / reset / mask IRQ ...，顺序由设备定）
  ↓
stop
  ↓
revoke_component_resources(id) —— Core 兜底，收回剩余 authority
  ↓
ResourceDomain becomes empty
```

#### Forced containment（强制隔离）

组件 crashed / hung / 恶意行为时：

```text
Component Failed
  ↓
Core containment               —— 阻止它继续访问资源
  ↓
reset / isolate device（尽可能）
  ↓
force revoke authority
  ↓
revoke_component_resources(id) —— 收回 authority-backed resources（handles）
```

> 强制隔离回收的是 **authority-backed 资源（handle）**。堆内存的清理走正常 Drop 路径；完整的内存回收属于 ExecutionDomain 的职责（见 §4）——phase 1 的 KernelNative 组件不承诺内存回收。

**意义**：restart、replace、fault recovery 全部建立在"Core 最终能收回 ResourceDomain"这一保证上。

## 4. ExecutionDomain —— 这里才真的有 enum

- **ResourceDomain** 回答"它拥有什么"；
- **ExecutionDomain** 回答"它在哪里运行"。

**两个 Domain 的形态故意不对称**：ResourceDomain 无 struct（§3，一个视图）；ExecutionDomain 是真正 owning 的 enum —— 它代表需要建立、切换、最终销毁的运行环境：

```rust
pub enum ExecutionDomain {
    KernelNative,
  IsolatedNative(AddressSpaceId),
    // future: Wasm(WasmInstance)
}
```

> **契约不能 ABI 锁定**：Interface 和 Handle 的定义必须与"传输方式"解耦，否则未来无法把组件挪进独立域。

### 4.1 不塞进 ComponentRecord

`ComponentRecord`（现状：`{id, name, state, entry, base}`）本质是 Registry / monitor / inspection 用的 metadata；`AddressSpace` 是 heavyweight runtime 对象。两者不混：

```rust
pub struct ComponentRecord {
    pub id: ComponentId,
    pub name: Vec<u8>,
    pub state: ComponentState,
    pub execution_kind: ExecutionKind,   // 只加一个轻量种类字段
}

#[derive(Clone, Copy, Debug)]
pub enum ExecutionKind {
    KernelNative,
  IsolatedNative,
}

真正 runtime：

```rust
pub struct ComponentRuntime {
  pub id: ComponentId,
  pub image: LoadedComponent,
  pub execution: ExecutionDomain,
}
```

分工：

```text
Registry —— "系统里有哪些 Component，是什么状态"（metadata）
Runtime  —— "这个 Component 现在实际占着什么运行环境"（runtime）
```

### 4.2 ComponentManager（未来把 loader / registry / execution 串起来）

```rust
pub struct ComponentManager {
  registry: Registry,
  runtimes: Vec<ComponentRuntime>,
}

impl ComponentManager {
  pub fn load(&mut self, name: &[u8], blob: &[u8], kind: ExecutionKind)
    -> Result<ComponentId, ComponentError>;
  pub fn start(&mut self, id: ComponentId) -> Result<(), ComponentError>;
  pub fn stop(&mut self, id: ComponentId) -> Result<(), ComponentError>;
  pub fn fail(&mut self, id: ComponentId, reason: ComponentError);
}
```

（概念代码；落地时按现有 Registry 状态机 `Declared → Starting → Ready + Failed`
接轨。当前 unload 只删记录、不释放放段内存。）

### 4.3 KernelNative 具体是什么

`KernelNative` 只需要保存已加载镜像和执行种类，调用方式与现在一致：

```rust
let ret = loader::call_init(&runtime.image);
```

### 4.4 IsolatedNative 的 AddressSpace（未来 C10）

M0.5 的静态启动页表不是这里的 AddressSpace。真正的运行期地址空间在需要
U-mode、故障隔离或可执行回收时才引入，由 Core 的 AddressSpaceManager 统一管理。

```text
Core AddressSpaceTable
└── AddressSpaceSlot
  ├── owner / generation / lifecycle
  ├── semantic mappings
  └── opaque ArchSpace
    └── Sv39 root / PTE pages
```

`ExecutionDomain` 只保存 `AddressSpaceId`，不拥有可以绕过 Core 修改映射的页表
对象。Core 保存地址空间的语义真相；PTE 只是 backend 的硬件投影。

Core 公开入口只接受 `AddressSpaceHandle`、`FrameHandle`、虚拟页和抽象权限：

```text
map_page(caller, space_handle, virtual_page, frame_handle, permission)
  → validate handle / owner / overlap / permission
  → install backend mapping
  → commit mapping record and trace
```

物理帧 ownership 永远由 Core FrameTable 保存，不放进 PTE 的 RSW 字段；映射销毁
也不依赖 `Drop` 扫页表。私有、借用和共享关系由 Core 的 frame owner 与 grant
记录表达，页表 backend 只负责安装、撤销和激活硬件映射。

映射和销毁必须是 Core 控制的显式事务：地址空间进入 `Dying` 后拒绝新操作，
停止引用它的任务，确认没有 CPU 正在使用，再由 backend 销毁页表，最后由 Core
按 ownership 回收资源并递增 generation。

具体的 backend contract 和 `ArchSpace` 所在 crate 仍需遵守当前依赖方向；在真正
实现 C10 前，不把 Core 绑定到 `Sv39`、`Pte`、`satp` 或某个 Arch crate。

### 4.8 Loader 自然分叉

现状 `load_component(blob, expected_machine)` 内部直接 `memory::alloc_frame()`，拿 PA 当 VA 拷贝，返回 `LoadedComponent { base, entry, text_size }`。未来：

```rust
fn load_component(blob: &[u8], target: &mut dyn LoadTarget)
    -> Result<LoadedComponent, LoaderError>
```

- KernelNative target：alloc frame → identity / kernel VA → copy；
- IsolatedNative target：向 Core 请求 frame authority → Core 提交映射 → copy。

loader 不需要知道 satp / Sv39 / KernelNative / IsolatedNative，它只知道"给我一块能放 section 的 memory"——保持 arch / mechanism 分层。

### 4.9 失败与退出的实际代码流

```rust
pub fn fail_component(&mut self, id: ComponentId, reason: ComponentError) {
    self.registry.mark_failed(id).unwrap();
    self.stop_component_tasks(id);
    revoke_component_resources(id);
    self.drop_runtime(id);   // KernelNative: drop Rust state 结束；
                 // IsolatedNative: Core 显式销毁 AddressSpace
}
```

正常退出：

```rust
match run_component(id) {
    Ok(()) => {}
    Err(err) => shutdown(id),
}
```

graceful path：

```text
Component 返回 Err
  ↓
Quiesce
  ↓
Component shutdown()
  ↓
Rust Drop
  ↓
绝大部分 Handle/Lease 自己释放
  ↓
revoke_owner(id)   ← 只是保险："还有没释放的 authority？有就 Core 扫掉。"
```

### 4.10 代码结构（目标形态）

```text
component/
├── mod.rs
│   ├── ComponentId
│   ├── ComponentState
│   └── ExecutionKind
│
├── registry.rs
│   └── ComponentRecord
│       ├── id
│       ├── name
│       ├── state
│       └── execution_kind
│
├── manager.rs                 ← 以后新增
│   ├── ComponentManager
│   └── ComponentRuntime
│       ├── LoadedComponent
│       └── ExecutionDomain
│
└── loader.rs
    └── load_component()


execution/
├── mod.rs
│   └── ExecutionDomain
│       ├── KernelNative
│       └── IsolatedNative(AddressSpaceId)
│
└── address_space.rs
  └── AddressSpaceManager / AddressSpaceSlot（未来 C10）


irq.rs       —— IrqTable，record 带 owner: ComponentId
mmio.rs      —— MmioTable，record 带 owner: ComponentId
dma.rs       —— DmaTable，record 带 owner: ComponentId

handle.rs    —— 类型化 Handle<...> + Slot{generation, owner, object}
```

ownership 结构：

```text
ComponentManager
      │
      ├── Registry
      │       └── metadata
      │
      └── ComponentRuntime
              ├── LoadedComponent
              └── ExecutionDomain
                     │
               ┌─────┴─────┐
               │           │
          KernelNative   AddressSpaceId
                        │
                Core-controlled backend
```

而 ResourceDomain 根本不在这棵树里：

```text
IRQ table  ─ owner=A ─┐
MMIO table ─ owner=A ─┼── ResourceDomain(A)
DMA table  ─ owner=A ─┘
```

### 4.11 落地顺序：现在只做两小步

1. **先不要写 ResourceDomain**。等 MMIO/IRQ 真正开始做的时候，在每个 authority record 上加 `owner: ComponentId`，再留一个 `revoke_owner(ComponentId)` 就够了；
2. **C8 完成 FrameHandle 后、真正需要隔离执行时**，再引入 `AddressSpaceManager`。
  它维护 `AddressSpaceSlot`、generation、语义 mapping ledger，并通过 Core 控制的
  backend 完成 map/unmap/activate/destroy；不使用 RSW ownership，也不依赖 `Drop`
  扫页表释放帧。

## 5. 生命周期

所有组件共享统一生命周期（但**不共享**业务接口）：

```text
Declared → Resolved → Starting → Ready → Quiescing → Stopped → Destroyed
                                ↘
                                Failed（运行中失败，任何阶段都可能进入）
```

| 状态 | 含义 |
|---|---|
| Declared | 系统知道这个组件存在 |
| Resolved | 所有 requires 都已找到 provider |
| Starting | 正在初始化 |
| Ready | 可以对外提供 Interface |
| Quiescing | 停止接受新请求并清理已有状态 |
| Stopped | 已停止 |
| Destroyed | 生命周期结束 |
| Failed | 运行过程中失败（可触发恢复流程） |

> **Failed 的恢复 = 逻辑重启**：标记 Failed、停止调度、在 Core 边界阻断过期访问、启动全新实例。phase 1 不承诺内存回收（KernelNative 无隔离）；完整回收留给未来 ExecutionDomain。目标上暂无 panic recovery（panic=abort），phase 1 用 Result 传播错误。

## 6. Ownership Tree 与 Dependency DAG —— 两种关系，绝不混淆

整个系统**不是一棵树**，而是两种关系的叠加：

### Ownership Tree（生命周期归属）

描述"谁创建谁、谁负责谁的生命周期"：

```text
VFS
├── Mount /
│   └── Ext4 #0
└── Mount /boot
    └── Fat #0
```

### Dependency DAG（Interface 依赖）

描述"谁依赖谁提供的 Interface"：

```text
        VFS
         │
       Ext4
      /    \
PageCache  BlockDevice
               │
              NVMe
```

PageCache 还可能同时被 VM 使用，形成跨子树共享 —— 这在树里表达不了，必须用 DAG。

> 陷阱：把 Dependency DAG 当 Ownership Tree 处理（删除组件时按依赖删），或反之（按树授权接口），都会出大问题。两套关系分别维护。

## 7. Composite Component

组件可以包含内部组件。例如 VFS：

```text
VFS
├── Namespace
├── Mount Manager
├── Dentry Cache
├── PageCache
├── Writeback
├── Ext4
├── FAT
├── Tmpfs
└── Procfs
```

内部结构**不要求第一天固定死**。例如 PageCache 若后来同时被 VFS、VM、mmap、exec 共享，就提升为独立 Component / Service。提升是常规演进操作，不是重构灾难 —— 这正是组件化的价值。

## 8. 替换与恢复（Recovery / Replace Policy）

不同组件可以有不同恢复能力：

| 类别 | 含义 | 例子 |
|---|---|---|
| Static | 不可替换 | Arch、极底层机制 |
| Restartable | 可重启（能重新进入 Ready；是否保留旧 semantic state 由组件 recovery contract 决定） | Scheduler、FileSystem、Network stack |
| Replaceable | 可整体替换 | Logger、Debug 组件 |
| Ephemeral | 临时存在 | 一次性工具组件 |

> **Restartable ≠ state-preserving restart。** 例如：
> - Scheduler restart → task 还在、runqueue 重建，基本无语义损失；
> - TCP stack restart → 服务能重新起来，但旧 connections 可能全部死亡 —— 仍然是 Restartable；
> - Ext4 restart → 从 block device + journal 重建，可恢复大量 semantic state。
>
> 定义：组件能够重新进入 Ready 状态；是否保留旧的 semantic state 由该组件自己的 recovery contract 决定。

### 第一阶段替换流程（不做热迁移）

```text
quiesce → stop → unbind → reset → replace → bind → start
```

允许**短暂中断**。这一流程的价值已经足够：替换一个调度器/分配器/驱动时，系统不需要重启。
复杂 live state migration 明确留到以后。

### 为什么策略组件可以安全 reset

Scheduler 丢失 runqueue 不要紧：从 Core 的 Truth（Runnable 任务列表）重新构造。

但要求是 **reconstructible to a safe state, not necessarily an equivalent state**：
Derived 状态（vruntime、LRU history、RTT 估计等）丢失后，系统必须能继续**安全正确**运行，
但短期行为、性能、策略连续性可能不同 —— 这不会破坏 safety、不会导致资源账本错误。
这是 restart / replace 成立的根本原因（状态分类见 `core-philosophy.md` §2）。

## 9. 完整示例：VFS 组件图

```text
                      VFS
                       │
          ┌────────────┼────────────┐
          │            │            │
        Ext4          FAT         Tmpfs
          │            │
          └─────┬──────┘
                │
            PageCache
                │
            Writeback
                │
           BlockDevice
                │
              NVMe
```

- VFS 对外：`provides FileSystemService`；
- Ext4：`requires BlockDevice, PageCache`，`provides FileSystem`；
- Mount ≈ 把一个 FileSystem provider attach 到 VFS namespace；
- 最底层 NVMe 是驱动组件：拿 Core 的 Handle，向上提供 BlockDevice。

这一张图浓缩了全部模型：分层依赖（DAG）、驱动作为组件、Interface 语义化、以及未来把任意节点换成不同实现/执行域的可能性。

## 10. 参考（详见 references.md）

- Theseus：细粒度组件与状态归属、生命周期/替换建模；
- RedLeaf：语言级隔离与驱动恢复（ResourceDomain 回收）；
- Singularity：契约式通信（Interface 语义化）；
- Wasm Component Model：跨 ABI 接口描述（未来）；