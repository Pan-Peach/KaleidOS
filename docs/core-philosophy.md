# 核心哲学（core-philosophy.md）

本文档解释 KaleidOS 最重要的设计原则：**什么状态属于 Core、什么属于 Component，以及为什么**。
这是整个仓库最不可违背的部分 —— 代码可以重写，哲学不要漂移。

## 1. 一句话原则

> **Core owns global resource truth AND the mechanisms required to preserve that truth and make forward progress. Components own replaceable semantics, policy, and derived state.**
> Core 保存真实且不可撒谎的系统状态，并拥有**为保存这份真相、推进 Core 自身资源/生命周期操作所必需的机制**；Component 实现可替换的算法、策略、协议和高级 OS 语义。
>
> 注："forward progress" 指 Core 自身资源/生命周期操作的推进，**不是**继续提供应用服务。

这句口号要读得更严谨一点：

> **Core owns resource truth and cross-component safety truth. Components may own their own semantic truth.**
> 中文：Core 掌握**资源真相**和**跨组件安全真相**；Component 可以拥有自己领域内的**语义真相**。

"Core owns truth" 不意味着"全世界一切权威状态都得进 Core"——VFS 的 mount 表、TCP 的连接状态、
POSIX 的 fd table 都是 Component 自己的业务真相（见 §2 状态四级分类），它们不属于 Core，
但也不是"丢了可以随便重建"的东西。

配套的第二句话：

> **Policy proposes, Core validates and commits.**
> 策略可以提议任何事，但只有 Core 验证通过后，真实资源才会被改动。
>
> 注意：这条只适用于**可替换的策略组件**（如调度器）。物理帧分配是 Core 内部机制，不是"提议"的策略 —— 见 §3 分配示例。

## 2. 状态归属：谁存什么

### Core 保存（真相）

| 状态 | 例子 |
|---|---|
| 对象存在性 | Task #7 存在；Frame #100 存在 |
| 对象状态 | Task #7 是 Runnable |
| 所有权 | Frame #100 属于 Component A；IRQ #5 已分配给 virtio-net |
| 执行位置 | Task #7 当前运行在 CPU 0 |
| 生命周期 | Component 处于哪个阶段；Handle 是否还有效 |

### Component 保存（策略/算法私有状态）

| 组件 | 保存 | 来源 |
|---|---|---|
| Scheduler | runqueue、RR cursor、vruntime | 调度策略私有 |
| VFS | mount 表、dentry cache | 语义私有 |
| Ext4 | inode 缓存、位图 | 格式私有 |

**判断方法**：把状态从组件里拿走，组件还能不能工作？—— 不能（runqueue 被删调度器没法转）。把状态从 Core 里拿走，系统会不会被骗？—— 会（所有权记录没了，两个组件可能同时用一块帧）。前者归 Component，后者归 Core。

### 状态四级分类：Core Resource Truth / Component Semantic State / Derived / Ephemeral

把"这个状态到底放 Core 还是 Component"细化为四类：

| 分类 | 定义 | 例子 | 归属 |
|---|---|---|---|
| **Core Resource Truth** | 真实世界的资源事实；错了会破坏**跨组件资源安全** | Frame owner、Task state、运行 CPU、AddressSpace 映射、资源所有权、Handle 有效性、IRQ 所有权 | Core |
| **Component Semantic State** | 某个 Component 自己负责的"业务真相"；**不能随便丢**，但也不是 Core 的责任 | VFS mount 表、TCP 连接状态、POSIX fd table、文件系统事务状态、game runtime 会话状态 | Component |
| **Derived** | 从权威状态构造的策略/加速状态；允许丢失，但丢失后必须能恢复到 **safe usable state**（不要求行为完全等价） | runqueue、LRU list、CFS vruntime、缓存索引 | Component |
| **Ephemeral** | 丢失完全不影响正确性的短暂状态 | debug buffer、临时统计、部分 trace 聚合 | Component |

判断规则：

```text
这个状态完全丢失以后：
  会让系统不知道真实资源世界是什么样？    → Core Resource Truth     → Core
  是某组件领域内的业务真相，不能随便丢？  → Component Semantic State → Component
  能从权威状态构造出正确但可能不同的状态？ → Derived                 → Component
  丢掉完全不影响系统正确性？              → Ephemeral               → Component
```

关键边界：

- **不属于 Core ≠ 可以丢了重建。** 不属于 Core 只代表"它不会破坏全局资源安全"；
  Component Semantic State（mount 表、TCP 连接、fd table）可能完全无法从 Core 的
  Task / Frame / Handle / IRQ 推导出来，是组件自己必须认真维护的语义真相；
- Derived 丢失后可能降低性能、改变短期行为、降低策略连续性（例如 CFS 丢失 vruntime、
  网络栈丢失 RTT 估计）—— 但这**不会破坏 safety、不会导致资源账本错误**；
- 这个分类以后是判断"字段到底该放 Core 还是 Component"的重要工具。

## 3. Policy proposes, Core validates and commits

### 为什么必须有验证

策略组件是**可以犯错的**：调度器可能提议一个已退出任务、分配器可能提议一块已被占用的帧、驱动可能用一个过期 handle。
Core 的存在意义就是：**错误提案可以发生，但绝不能生效**。

### 调度示例

```text
Scheduler 提议：下一步运行 Task #7
Core 验证：
  - Task #7 存在？
  - Task #7 是 Runnable？
  - Task #7 没有在别的 CPU 上运行？
通过 → commit（真正切换上下文），并记录 trace
拒绝 → 记录 Core rejection，调度器自行修正
```

### 分配示例

物理帧分配是 **Core 内部机制**（canonical，不热卸载；可能按 build/profile 选择实现）。请求者不"提议"帧号，而是向 Core 要一帧：

```text
请求者：请给我一帧
Core 的分配器：
  - 选择一帧（如 Frame #100）
  - 验证：Frame #100 存在？是 Free 状态？请求者有权限？
  - commit ownership（从此 Frame #100 归请求者）
  - grant authority（FrameHandle）
```

未来若引入 `MemoryPolicy` 组件，它只能**提议偏好**（如 NUMA 偏好、配额），最终选择/验证/提交仍在 Core。

### 工程含义

- 任何对真实资源的操作，代码路径上必须有一个"Core 验证点"；
- 验证点要有 trace（proposal 事件 + 结果），这是调试与 CoreTest 的基础（见 testing.md）；
- 策略组件因此可以随时 reset / 替换：它丢的是 Derived 状态（runqueue 等），从 Core 的 Truth 重新构造即可 —— 但重建目标是**安全可用状态，而非完全等价状态**（见 §2 状态分类）。

## 4. Authority ≠ Interface

不要把所有东西都叫 capability。两个概念必须分开：

### Resource Authority —— "你有权动什么"

由 Core 产生、不可伪造、最终由 Core 验证：

```text
FrameHandle  MmioHandle  IrqHandle  DmaHandle
TaskHandle   TimerHandle AddressSpaceHandle
```

**硬性要求**：驱动永远不应该拿到裸物理地址、裸 IRQ 号、裸 DMA 指针或任意 MMIO 指针。
它应该拿到 `MmioHandle`、`IrqHandle`、`DmaHandle` —— 通过 handle 间接访问，Core 在中间校验。

### Interface —— "你能提供什么"

由组件声明、供其他组件消费：

```text
Device:   BlockDevice  NetDevice  InputDevice  DisplayDevice  AudioDevice
Service:  FileSystemService  NetworkService  GraphicsService  LoggerService
Policy:   SchedulerPolicy  PageReplacementPolicy  （未来：MemoryPolicy）
```

### 两者关系

```text
Core
 │  grant Resource Authority（Handle）
 ▼
NVMe Component
 │  provides
 ▼
BlockDevice（Interface）
```

- Handle 是**权限凭证**，Interface 是**能力契约**；
- 一个组件可以"有权动硬件"（有 Handle）但"不对外提供能力"（没有 Interface），反之亦然；
- 驱动协议逻辑因此可以脱离 ISA：Native NVMe / Wasm NVMe / Fake NVMe 只要都提供 BlockDevice，上层无感知。

## 5. 什么应该进 Core（判断标准）

新增任何东西进 Core 之前，问三个问题：

1. **它是不是真相？**（对象存在性 / 状态 / 所有权 / 生命周期 —— 是）
2. **一个错误的 Component 能否通过它破坏全局不变式？**（能 → 考虑收回 Core；不能 → 留在组件层）
3. **它是不是"基础功能"？**（是 → 警惕！"基础功能"恰恰容易误入 Core。Core 收的是 authority，不是功能）

核心判据（litmus test）：

> 如果一个完全错误的 Component 能通过某个 API 破坏其他 Component 或全局 invariant，
> 那么应该缩小 API，或者把最终 authority 收回 Core。

**反例自查**：RR 算法放进 Core？—— 不需要。它错了只会调度得烂，不会让两个任务同时占一个 CPU（检查在 Core）。物理帧分配则相反：它是 Core 内部机制 —— 分配错了会破坏所有权真相，必须由 Core 掌握（见 §3 分配示例）。

## 5.5 内存：无 per-component 记账

- **Core 与组件共享一个 Core heap**：没有 per-ComponentId 的字节计费，没有 per-component arena / 私有堆；
- **ResourceDomain 记录的是 authority handle**（Mmio / Irq / Dma / Frame ...），用于保护与 revoke，**不是**内存字节数；
- 因此组件失败时，Core 不承诺回收其堆内存（见 §5.6 组件失败语义）。

## 5.6 组件定义（4 项测试）

一个东西是否算"Component"，用四项测试判定：

1. **裸 Core 能否在缺少它时存活？**（Core 不依赖它也能推进自身资源/生命周期操作）
2. **它死了，Core 的真相是否仍然完整？**（它不持有 Core 的全局资源真相）
3. **它能否真正 stop / unload？**（有明确的停止/卸载路径）
4. **它的状态是否 loss-tolerant / 可重建？**（丢失后能重建到 safe usable state）

> 组件失败 = **逻辑死亡、物理驻留**：标记 Failed、停止调度、在 Core 边界阻断过期访问、启动全新实例（逻辑重启）。phase 1 不承诺内存回收（KernelNative 无隔离）；完整回收留给未来 ExecutionDomain（Wasm / 地址空间）里程碑。目标上暂无 panic recovery（panic=abort），phase 1 用 Result 传播错误。

## 6. 由哲学推导出的工程约束

- **Core 必须 host-testable**：真相逻辑不能依赖 QEMU 才能验证（见 testing.md）；
- **CoreTest 没有 god-mode**：测试组件也只能走真实 Core API，不能改 Core 私有状态；
- **接口是语义，不是调用方式**：Rust trait + direct call 只是第一阶段的传输绑定；
- **组件注册静态**：第一阶段不做动态加载，架构先立住，再谈弹性；
- **Core 词汇表保持最小**：每加一个 Core API，都是一份必须永远保持正确、可验证的承诺。

## 7. 哲学来源（详见 references.md）

- **Exokernel**：保护与管理分离 —— "Core 保护资源，Component 决定策略"的思想源头；
- **Asterinas**：策略移出 TCB、策略输出必须验证 —— propose/validate 的工程先例；
- **seL4**：typed authority、不可伪造 capability —— Handle 类型化的直接来源；
- **Theseus / RedLeaf**：明确的状态归属、资源回收 —— ResourceDomain 思想；
- **SPIN**：类型安全组件可以安全运行在内核地址空间 —— KernelNative 域的依据。