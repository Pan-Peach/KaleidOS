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
    IsolatedNative(AddressSpace),
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
```

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

（概念代码；落地时按现有 Registry 状态机 `Declared → Starting → Ready + Failed` 接轨。当前 unload 只删记录、不释放放段内存。）

### 4.3 KernelNative 具体是什么

甚至什么都不用存：

```text
ComponentRuntime
├── LoadedComponent
└── ExecutionDomain::KernelNative
```

调用方式与现在完全一样（loader 的 `call_init`）：

```rust
let ret = loader::call_init(&runtime.image);   // extern "C" fn() -> i32
```

以后只是把 `0 = Ok / 非 0 = Err` 改得正式一点。

### 4.4 IsolatedNative 才多一个 AddressSpace

这里的 `AddressSpace` 是 ExecutionDomain 使用的运行时机制，不是 Component
直接操作的 Sv39 页表对象。上层只表达 region、frame authority 和 permission；
Core 负责校验 ownership/handle，具体 PTE、VPN、`satp`、TLB 操作由 arch
backend 完成。当前只需要支持 Sv39，接口不要因此把 Core 绑定到 Sv39。

```rust
pub struct AddressSpace {
    root: FrameId,   // 第一版甚至可以只放这一个字段
}

impl AddressSpace {
    pub fn new() -> Result<Self, VmError>;
    pub fn map_owned(&mut self, va: VirtAddr, frame: FrameId, flags: MapFlags) -> Result<(), VmError>;
    pub fn map_borrowed(&mut self, va: VirtAddr, frame: FrameId, flags: MapFlags) -> Result<(), VmError>;
    pub fn activate(&self);
}
```

### 4.5 RSW bit —— ownership 直接贴在 PTE 上

不需要维护 `Vec<Mapping>`：Sv39 PTE 的 **RSW 字段是 supervisor software 保留、硬件忽略**的（RISC-V Privileged Spec [Supervisor-Level ISA, v1.13](https://docs.riscv.org/reference/isa/priv/supervisor.html)），正好留给我们：

```text
PTE

| PPN | RSW | D A G U X W R V |
        ↑
        │
     OWNED bit (RSW[0])

map_owned    → PTE.RSW[0] = 1
map_borrowed → PTE.RSW[0] = 0
```

> 能由底层机制自己表达的东西，就别在 Core 上面再建一层账本 —— 这正是这几轮一直在做的减法。

### 4.6 AddressSpace::drop 真的能扫页表

```rust
impl Drop for AddressSpace {
    fn drop(&mut self) {
        destroy_page_table(self.root);
    }
}

fn destroy_page_table(table: FrameId) {
    for pte in table.entries() {
        if !pte.valid() {
            continue;
        }
        if pte.is_branch() {
            let child = pte.frame();
            destroy_page_table(child);
            free_frame(child);          // page-table page 永远属于 AddressSpace 自己
        } else if pte.owned() {
            free_frame(pte.frame());    // OWNED frame：free
        }
        // BORROWED mapping：只取消映射，不 free PA
    }
    free_frame(table);
}
```

于是之前"Arc cycle / mem::forget / Box::leak / 组件 allocator"的纠结全部消失：

```text
drop AddressSpace → 扫 PTE → OWNED frame 全 free
```

### 4.7 Shared memory 第一版不做

IsolatedNative 第一阶段只有 OWNED / BORROWED 两种：

- **Owned**：heap / stack / private data / private code —— AddressSpace 死了直接 free；
- **Borrowed**：Core trampoline / MMIO / kernel shared code —— AddressSpace 死了只 unmap。

将来真要 SharedMemory 再引入 `SharedMapping`，RSW 可扩展为 `00 borrowed / 01 owned / 10 shared`，或 shared 单独做 metadata / refcount。**不要提前解决。**

### 4.8 Loader 自然分叉

现状 `load_component(blob, expected_machine)` 内部直接 `memory::alloc_frame()`，拿 PA 当 VA 拷贝，返回 `LoadedComponent { base, entry, text_size }`。未来：

```rust
fn load_component(blob: &[u8], target: &mut dyn LoadTarget)
    -> Result<LoadedComponent, LoaderError>
```

- KernelNative target：alloc frame → identity / kernel VA → copy；
- IsolatedNative target：alloc frame → `map_owned(frame, component_va)` → copy。

loader 不需要知道 satp / Sv39 / KernelNative / IsolatedNative，它只知道"给我一块能放 section 的 memory"——保持 arch / mechanism 分层。

### 4.9 失败与退出的实际代码流

```rust
pub fn fail_component(&mut self, id: ComponentId, reason: ComponentError) {
    self.registry.mark_failed(id).unwrap();
    self.stop_component_tasks(id);
    revoke_component_resources(id);
    self.drop_runtime(id);   // KernelNative: drop Rust state 结束；
                             // IsolatedNative: Drop → AddressSpace → 页表 walk → free OWNED
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
│       └── IsolatedNative(AddressSpace)
│
└── address_space.rs
    └── AddressSpace


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
         KernelNative   AddressSpace
                             │
                          page table
```

而 ResourceDomain 根本不在这棵树里：

```text
IRQ table  ─ owner=A ─┐
MMIO table ─ owner=A ─┼── ResourceDomain(A)
DMA table  ─ owner=A ─┘
```

### 4.11 落地顺序：现在只做两小步

1. **先不要写 ResourceDomain**。等 MMIO/IRQ 真正开始做的时候，在每个 authority record 上加 `owner: ComponentId`，再留一个 `revoke_owner(ComponentId)` 就够了；
2. **Sv39 做完以后**，写一个非常薄的 `AddressSpace { root: FrameId }`，做到 `new / map_owned / map_borrowed / activate / Drop`。其中 **Drop → 页表 walk → free OWNED frame** 这一条跑通，ExecutionDomain 最核心的机制就出来了。

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