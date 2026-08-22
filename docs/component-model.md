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
| Policy | 可替换的策略算法 | SchedulerPolicy、FrameAllocatorPolicy、PageReplacementPolicy |

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

## 3. ResourceDomain

每个 Component 有一个由 Core 维护的资源域，记录它拥有的全部真实资源：

```text
virtio-net #7
owns:
    MmioHandle #3
    IrqHandle #5
    DmaHandle #8
    TimerHandle #11
```

### 组件停止时的回收顺序（Core 执行）

```text
quiesce                      —— 停止接受新请求，清理进行中状态
  ↓
stop
  ↓
Core revoke ResourceDomain
  ↓
IRQ mask → DMA revoke → MMIO revoke → Timer cancel → Resource release
  ↓
destroy
```

**意义**：restart、replace、fault recovery 全部建立在同一套 ResourceDomain 机制上 —— 只要回收是确定的，重建就是安全的。

## 4. ExecutionDomain

- **ResourceDomain** 回答"它拥有什么"；
- **ExecutionDomain** 回答"它在哪里运行"。

未来可能的执行域：`KernelNative`（内核地址空间 Rust 函数）、`UserAddressSpace`、`WasmSandbox`。
第一阶段只需要 `KernelNative`，但**契约不能 ABI 锁定**：Interface 和 Handle 的定义必须与"传输方式"解耦，否则未来无法把组件挪进独立域。

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
| Restartable | 可重启（状态可重建） | Scheduler、FileSystem、Network stack |
| Replaceable | 可整体替换 | Logger、Debug 组件 |
| Ephemeral | 临时存在 | 一次性工具组件 |

### 第一阶段替换流程（不做热迁移）

```text
quiesce → stop → unbind → reset → replace → bind → start
```

允许**短暂中断**。这一流程的价值已经足够：替换一个调度器/分配器/驱动时，系统不需要重启。
复杂 live state migration 明确留到以后。

### 为什么策略组件可以安全 reset

Scheduler 丢失 runqueue 不要紧：从 Core 的真相（Runnable 任务列表）重新扫描重建；
Buddy 丢失 free list 不要紧：从 Core 的帧表重建。**真相在 Core，策略状态永远可重建** —— 这是替换模型成立的根本原因。

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