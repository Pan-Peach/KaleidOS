# 核心哲学（core-philosophy.md）

本文档解释 KaleidOS 最重要的设计原则：**什么状态属于 Core、什么属于 Component，以及为什么**。
这是整个仓库最不可违背的部分 —— 代码可以重写，哲学不要漂移。

## 1. 一句话原则

> **Core owns truth. Components own policy and semantics.**
> Core 保存真实且不可撒谎的系统状态；Component 实现可替换的算法、策略、协议和高级 OS 语义。

配套的第二句话：

> **Policy proposes, Core validates and commits.**
> 策略可以提议任何事，但只有 Core 验证通过后，真实资源才会被改动。

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
| Buddy allocator | buddy tree、free lists | 分配算法私有 |
| VFS | mount 表、dentry cache | 语义私有 |
| Ext4 | inode 缓存、位图 | 格式私有 |

**判断方法**：把状态从组件里拿走，组件还能不能工作？—— 不能（runqueue 被删调度器没法转）。把状态从 Core 里拿走，系统会不会被骗？—— 会（所有权记录没了，两个组件可能同时用一块帧）。前者归 Component，后者归 Core。

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

```text
Buddy 提议：分配 Frame #100
Core 验证：
  - Frame #100 存在？
  - 是 Free 状态？
  - 请求者有权限？
通过 → Core commit ownership（从此 Frame #100 归请求者）
```

### 工程含义

- 任何对真实资源的操作，代码路径上必须有一个"Core 验证点"；
- 验证点要有 trace（proposal 事件 + 结果），这是调试与 CoreTest 的基础（见 testing.md）；
- 策略组件因此可以随时 reset / 替换：它丢了 runqueue 不要紧，从 Core 的真相重新扫描重建即可。

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
Policy:   SchedulerPolicy  FrameAllocatorPolicy  PageReplacementPolicy
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

**反例自查**：Buddy 算法放进 Core？—— 不需要。它错了只会浪费内存，不会破坏所有权真相（真相在 Core 的帧表里）。RR 算法放进 Core？—— 不需要。它错了只会调度得烂，不会让两个任务同时占一个 CPU（检查在 Core）。

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