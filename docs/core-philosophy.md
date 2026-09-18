# 核心哲学（core-philosophy.md）

本文档解释 KaleidOS 最重要的设计原则：**什么状态属于 Core、什么属于 Component，以及为什么**。
这是整个仓库最不可违背的部分 —— 代码可以重写，哲学不要漂移。

## 0. 总原则：少即是多

KaleidOS 的目标不是构造一个"功能完整的内核"，而是构造一个**足够小、足够稳定、能够支撑多种系统形态的 Core**。

> Core 做得越少，KaleidOS 能形成的系统形态越多。

因此判断某项能力是否应该进入 Core 时，默认答案应当是：**如果它可以安全、清晰地外置，就不应该放进 Core。** Core 不应该因为"传统内核一般这么做"而拥有某项功能，也不应该为了方便某一个部署形态，把策略永久固化在最底层。

KaleidOS 的适应性来自：**稳定且极小的机制 + 可替换的上层策略与组件。** Core 的价值不是"什么都能做"，而是提供那些**无法再向外推的最小机制和边界**。

## 1. 一句话原则

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

更完整地说，Core 的职责是系统中的 **authority / resource / isolation / lifetime arbiter**：它回答"谁拥有这个资源、谁可以访问、当前 authority 是否仍有效、资源如何被授予/转移/撤销、一个 execution domain 能看见哪些内存、IRQ / DMA / MMIO 的硬件边界如何建立、一个组件死亡后哪些资源必须失效、一个任务如何被切换、一个 fault 应该终止哪个 execution domain"。

而 Core **不决定**：用什么调度策略；网络栈 / 文件系统如何设计；驱动用什么框架；服务如何组合；POSIX 如何实现；某个组件采用什么内部数据结构；某个系统必须采用宏内核、微内核还是用户态服务形态。

> **Core provides mechanism, not policy.** 以调度为例：Core 提供 context switch / task state / timer / runnable mechanism / primitive scheduling hooks；RR / priority / EDF / 自定义策略都属于 Scheduler 组件（见 §3 与 `component-model.md`）。

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
| **Core Resource Truth** | 真实世界的资源事实；错了会破坏**跨组件资源安全** | PhysicalRegion 占用与归属、Task state、运行 CPU、AddressSpace 映射、资源所有权、Handle 有效性、IRQ 所有权 | Core |
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

  Component Semantic State（mount 表、TCP 连接、fd table）可能完全无法从 Core 的
  Task / Frame / Handle / IRQ 推导出来，是组件自己必须认真维护的语义真相；
  网络栈丢失 RTT 估计）—— 但这**不会破坏 safety、不会导致资源账本错误**；

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

### 内存区域分配示例

物理内存分配是 **Core 内部机制**（canonical，不热卸载；可能按 build/profile 选择实现）。请求者不"提议"物理地址，而是向 Core 要一段区域：

```text
请求者：请给我一帧
Core 的分配器：
  - 选择一个 PhysicalRange
  - 验证：区域存在？空闲？范围合法？
  - commit region ownership
  - grant memory region / address-space authority
```

未来若引入 `MemoryPolicy` 组件，它只能**提议偏好**（如 NUMA 偏好、配额），最终选择/验证/提交仍在 Core。

### 工程含义


## 3.5 不是微内核，也不是宏内核：信任等级决定边界

KaleidOS 不追求某种纯粹的内核教条，也不再要求"Core 中绝不能出现任何看起来像宏内核的东西"。真正应该坚持的是：**不同信任等级使用不同边界。**

如果一个组件已经被完全信任，强迫它经过昂贵的 capability / syscall / copy / validation 路径，并不会让系统更"纯洁"，反而会破坏 KaleidOS 的适应性。因此允许至少三种执行 / 信任域：

| 域 | 特权级 | 地址空间 | 信任模型 |
|---|---|---|---|
| KernelNative | S | 共享内核 AS | 完全受信任；同特权级直接 native call，零切换成本 |
| IsolatedNative | S | 私有 AS | 半信任；native 性能 + 条件性故障隔离（可选实验，非里程碑） |
| U-mode（SandboxedNative） | U | 私有 AS | 不信任；syscall 边界，硬件强制隔离 |

**部署形态本身就是安全策略的一部分**：不信任它，就不要部署成 KernelNative。Core 不应该靠"所有组件都经过同样重的安全机制"来解决信任问题。KernelNative 的安全边界不是"防御恶意组件"，而是 API 边界、ownership、lifetime、authority bookkeeping 与可撤销资源身份——它仍可能通过裸指针、非法内存写、UB 破坏整个 Core。

**同一语义、不同传输**：allocate / map / irq / log / interface-call 这些语义可以复用，但传输方式必须分离——KernelNative 用窄 `extern "C"` ABI，IsolatedNative 走受控边界，U-mode 用独立的 syscall wire ABI。不要因为三个域最终都做"同一件事"，就强行让它们共享同一个底层 ABI。

（执行域与 Handle→Lease 细节见 `driver-model.md`；AddressSpace 注册表见 `component-model.md`。）

## 4. Authority ≠ Interface

不要把所有东西都叫 capability。两个概念必须分开：

### Resource Authority —— "你有权动什么"

由 Core 产生的类型化 token；token 可被伪造，authority 最终由 Core 验证：

```text
MmioHandle  IrqHandle  DmaHandle
TaskHandle  TimerHandle AddressSpaceHandle
```

**硬性要求**：驱动永远不应该拿到裸物理地址、裸 IRQ 号、裸 DMA 指针或任意 MMIO 指针。
它应该拿到 `MmioHandle`、`IrqHandle`、`DmaHandle` —— 通过 handle 间接访问，Core 在中间校验。

#### Handle 的真正意义：control plane，不是内存屏障

即使 KernelNative 可以接触裸地址，Handle 仍然有存在价值，但必须明确：**Handle 在 KernelNative 中不是内存安全屏障。** 如果一个受信组件已经拿到裸 MMIO pointer，撤销 handle 并不能神奇地让已泄漏的裸 pointer 停止工作。

Handle 的意义在 **control plane**：resource identity / ownership / authority / generation / lifetime / revocation / accounting / cleanup。它表达的是"你现在被授予了对这个资源的 authority"，而不是"CPU 从物理上绝不允许你绕过我"。因此 KernelNative 可以有这样的分层：

```text
control plane:  handle / authority
fast path:      经 Core 校验一次后派生的 native pointer / mapping（typed Lease）
```

Handle 负责：谁拥有设备；资源是否仍有效；teardown 时撤销谁；generation 防止 stale handle；restart 后旧 authority 失效；由 Core 统一管理资源生命周期。（`smoltcp` 式的直觉：handle 是稳定的身份与管理引用，而不是对象本身。）

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


## 5. 什么应该进 Core（判断标准）

新增任何东西进 Core 之前，问这组问题：

1. **它是不是真相？**（对象存在性 / 状态 / 所有权 / 生命周期 —— 是）
2. **它是否必须拥有系统级 authority 才能正确工作？**
3. **把它做成 component / library，会不会破坏安全边界？**
4. **它是 mechanism，还是 policy？**
5. **不同 KaleidOS 部署是否可能希望替换它？**
6. **它是否只服务某一个具体系统形态？**
7. **它进入 Core 后，会不会迫使其它 domain 也接受这个设计？**
8. **它能否通过一个更小的 primitive 暴露给上层？** 是不是因为"Linux / 传统 OS 都这么做"才想放进来？

如果最后的答案是"它只是方便放 Core"，那通常就不应该放。特别警惕"它是不是基础功能？"——"基础功能"恰恰最容易误入 Core：**Core 收的是 authority，不是功能**。

核心判据（litmus test）：

> 如果一个完全错误的 Component 能通过某个 API 破坏其他 Component 或全局 invariant，
> 那么应该缩小 API，或者把最终 authority 收回 Core。

**反例自查**：RR 算法放进 Core？—— 不需要。它错了只会调度得烂，不会让两个任务同时占一个 CPU（检查在 Core）。物理帧分配则相反：它是 Core 内部机制 —— 分配错了会破坏所有权真相，必须由 Core 掌握（见 §3 分配示例）。

## 5.5 内存：无 per-component 记账


#### 5.5.1 内存三分法

内存模型中需要分开讨论三件事：

```text
Physical Memory       RAM、物理帧、保留区、分配与 ownership
Protection             谁可以访问哪些 region，以及访问权限
Address Translation    VA 如何映射到 PA
```

`MemoryDomain` 是 Core 层的语义抽象，记录 domain 的资源归属、可见区域和
权限约束；它不等于 `PageTable`，也不携带 Sv39/Sv32-specific knowledge。

MMU 平台可以用 paged `AddressSpace` 同时提供翻译和硬件权限检查；NoMMU 平台
则可能只有 flat address space，再由 PMP、MPU 或其他机制提供 protection。
两者不强行伪装成同一个能力集合：没有 MMU 也不意味着拥有虚拟地址空间、
page fault、COW 或 lazy mapping。

Sv39、Sv32、PMP、MPU 都是 backend/mechanism。当前 RISC-V profile 已实现 Sv39 与
Sv32，未来仍可根据机器能力选择 backend；Core API 应使用 AddressSpace、PhysicalRange、
VirtualRange 和抽象 permission，裸 PA、PTE、VPN、`satp` 只属于 arch 层。

#### 5.5.2 定案：物理分配粒度 ≠ VM 映射粒度

**`memory::ALLOC_GRANULE`（物理分配粒度）与 `AddressSpaceBackend::GRANULE`
（VM 映射粒度）在语义上彻底解耦**（两者当前数值上都是 4 KiB，但这是巧合，
不是耦合）：

- `ALLOC_GRANULE`：buddy/区域分配器的最小单元，只属于**物理内存机制**；
- `GRANULE`：各翻译 backend 自己声明的对齐/步进规则。Core 的
  `KernelAddressSpace::validate` 用 `B::GRANULE` 做对齐校验，**不引用任何
  分配器常量**（`os/core/src/memory/address_space.rs` 的 `is_aligned::<B>()`）。

推论：**NoMMU 不存在 VM page 的概念**——恒等 backend 用 `GRANULE = 1`，
Core 校验自动退化为 no-op，Core 不需要写任何 `#[cfg]`（host 测试
`core_validation_accepts_unaligned_with_granule_one` 是验收点）。

## 5.6 组件定义（4 项测试）

一个东西是否算"Component"，用四项测试判定：

1. **裸 Core 能否在缺少它时存活？**（Core 不依赖它也能推进自身资源/生命周期操作）
2. **它死了，Core 的真相是否仍然完整？**（它不持有 Core 的全局资源真相）
3. **它能否真正 stop / unload？**（有明确的停止/卸载路径）
4. **它的状态是否 loss-tolerant / 可重建？**（丢失后能重建到 safe usable state）

> 组件失败 = **逻辑死亡、物理驻留**：标记 Failed、停止调度、在 Core 边界阻断过期访问、启动全新实例（逻辑重启）。phase 1 不承诺内存回收（KernelNative 无隔离）；完整回收留给未来 ExecutionDomain（Wasm / 地址空间）里程碑。panic 契约见 §5.8：phase 1 已实现 init 边界与任务边界的**协作式** panic containment（独立栈 + stack-switch escape，逻辑死亡，非 unwinding）；但这**不等于** fault isolation。

## 5.7 最小性不是代码高尔夫

"Core 越小越好"不是"代码越少越好"的代码高尔夫。真正含义是：**Core 只保留那些必须拥有全局 authority 才能正确完成的机制。** 典型的 Core 内容：

```text
resource authority        handle lifecycle          memory ownership
address-space primitive   task / context primitive  interrupt primitive
timer primitive           component loader          component lifecycle
interface registry primitive   fault routing        machine capability
```

可以独立选择策略的东西，尽量外置（见 §0）。不塞进 Core 的典型：某个调度算法、文件系统格式、网络协议栈、设备协议、POSIX 语义、ELF loader、Wasm runtime。

## 5.8 失败与 panic 的诚实契约

Phase 1 中，KernelNative component 的**普通失败**与**panic**必须区分：

- **普通失败**（可恢复的 component failure）：`Result` / status code / `kcomp_instance_create() != 0`（返回 `0 / -errno`）；
- **意外 panic**：不要假装拥有不存在的恢复能力。如果组件仍然直接跑在 Core 栈上（`Core stack → kcomp_init()`（**已删除**，现为 `kcomp_instance_create`）→ panic），`panic=abort` 不可能凭空形成 component recovery boundary。

因此诚实的契约是：**expected failure → return error；unexpected panic → 默认 fatal。** 若要"panic → 杀掉 instance → Core 继续"，组件必须先拥有一个**可独立丢弃的 execution context**（独立 task / 独立 stack / component trampoline / instance identity），panic handler 再进入 Core 的 abort 路径。

Phase 1 **已实现** init 边界与任务边界的协作式 containment：组件跑在 Core 拥有的独立栈上，panic 时先打印诊断、再 stack-switch 回 Core 上下文，由 Core 把该 task / instance 标记失败并重新调度（逻辑死亡；不 unwind）。**但必须明确：panic recovery ≠ fault isolation。** KernelNative 组件仍可能写坏 Core 内存、产生 UB、持有裸 pointer、在持锁状态死亡、破坏共享数据结构——因此 KernelNative 的 panic recovery 是 **cooperative failure containment**，不是对抗性隔离；真正的 memory fault containment 交给 `IsolatedNative` / U-mode。

失败 teardown 也必须以**资源生命周期**为核心（而非 `free(stack)` 就结束），并且 `CPU isolation ≠ DMA isolation`——细节见 `component-model.md` §3.3 与 `driver-model.md`。

## 6. 由哲学推导出的工程约束

- **默认外置**：新能力默认不进 Core；只有"无法安全外置的 authority 机制"才进（§0 / §5）。
- **显式 authority**：涉及 authority 的 Core 操作都显式接收 `RequestContext`（谁在请求 / 属于哪个 instance / 哪个 domain / 什么 rights），不偷偷读 `current_task()` 或全局 caller（见 `driver-model.md`）。
- **能力退化可见**：MMU / IOMMU / 特权级等平台能力差异必须显式可见，不能伪装（见 `architecture.md`）。
- **契约不绑 ABI / 传输**：Interface 与 Handle 和"怎么调用"解耦；组件边界使用稳定 C ABI，Rust ABI 永不成为组件 ABI（见 `architecture.md`）。
- **组件间只走 Interface registry**：禁止 flat ELF symbol 互链，组件间交互经 typed interface + explicit authority（见 `component-model.md` §2.1）。
- **失败可推理**：普通失败用 Result；panic 走协作式 containment；teardown 按资源生命周期回收（§5.8）。

## 7. 哲学来源（详见 references.md）

## 8. 最核心的一句话

> **KaleidOS 要统一的不是"系统长什么样"，而是 authority、resource identity、lifetime、execution primitives、isolation primitives、component lifecycle、explicit interfaces。** 它提供一个足够小的权力与资源核心，使不同信任模型、执行模型、策略与服务能在同一套基础机制之上自由组合——**少即是多。**
