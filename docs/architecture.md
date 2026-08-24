# KaleidOS 架构总览（architecture.md）

## 1. 项目定位

KaleidOS 是一个**组件化、多架构**的操作系统，面向学习、实验与个人创作。

- 它不是 Linux 的复刻，POSIX / BusyBox 兼容只是未来可选的一种 *Profile*；
- 核心目标是亲手实现 OS 的关键基础机制，并让 Core 以上的大部分 OS 功能成为**可组合的 Component**；
- 同一个 Resource Core 底座，通过重新组合 Component，可以长出完全不同的操作系统（One core, countless systems）。

> **A small resource-authority core beneath a composable graph of operating-system components.**
> 中文：一个小型资源权威 Core，以及构建在其上的可组合操作系统组件图。

## 2. 分层结构

```text
Applications / System Personality（应用 / 系统性格）
                │
         Services / Devices（服务 / 设备）
                │
            Components（组件）
                │
          Resource Core（资源权威核心）
                │
   Arch + Machine Discovery（架构 + 机器发现）
                │
            Hardware（硬件）
```

各层职责：

| 层 | 职责 | 谁实现 |
|---|---|---|
| Applications / System Personality | 最终用户形态（游戏、Shell、个人应用） | 未来 |
| Services / Devices | 跨组件聚合的系统服务（文件系统服务、网络服务、图形服务） | 组件图组合的结果 |
| Components | 可替换的算法、策略、协议、驱动 | 组件 |
| Resource Core | 真实资源的存在、状态、权限、生命周期；不可破坏的不变式 | 核心（最小可信基） |
| Arch + Machine Discovery | 消化机器差异（ISA 原语 / 机器发现） | 核心下方 |
| Hardware | 真实硬件或 QEMU | —— |

**分层原则**：Core 以下解决机器差异，Core 以上解决 OS 功能差异。

### 一组必须保持分离的边界（≠）

KaleidOS 最重要的边界不是"模块"，而是一组概念分离：

```text
Identity         ≠  Authority           —— 名字 ≠ 权限
Interface        ≠  Transport           —— 契约 ≠ 调用方式
ResourceDomain   ≠  ExecutionDomain     —— 拥有什么 ≠ 在哪里运行
Ownership Tree   ≠  Dependency DAG      —— 生命周期 ≠ 依赖关系
Machine Description ≠ FDT specifically  —— 机器发现 ≠ 某种具体机制
```

其中 Core owns Resource Truth（含跨组件安全真相）；Component 拥有 Semantic / Derived / Ephemeral State、Policy、Protocol、Semantics（状态四级分类见 `core-philosophy.md` §2）。

## 3. Arch 与 Machine Discovery

### arch/ —— 架构层（ISA 本身）

负责与指令集相关的原语：

- trap / exception 入口与分发
- 寄存器上下文与 context switch
- MMU / TLB 操作
- 用户态模式切换（user mode transition）
- 中断开关（interrupt enable/disable）
- 原子操作 / CPU 原语
- 固件调用原语（firmware-call primitives，如 RISC-V SBI 调用；**UEFI runtime 调用属于 Boot/Firmware environment，不是 ISA 属性**，不归 x86 arch 所有）

目录：`kernel/arch/riscv64`（每个 ISA 一个 crate）。后续：`x86_64` / `aarch64` / `loongarch64`。

### Machine Discovery —— 机器发现（不设 platform 层）

不设 platform 层：机器差异由 **Machine Discovery** 消化 —— 从各种来源发现
RAM、CPU topology、中断控制器、总线、MMIO 设备与固件设备信息，归一化为
KaleidOS 内部概念：`MachineInfo` / `DeviceDescriptor` / `MemoryRegion` / `CpuInfo`。

**FDT 只是 Machine Discovery 的一种 backend**（RISC-V / ARM 常用），未来还可能有：

```text
FDT      —— 设备树（当前唯一 backend，QEMU 经 a1 传入 DTB）
ACPI     —— x86 / ARM Server（未来）
UEFI tables / PCI bus probing / 其他 firmware description（未来）
```

- 中断控制器（PLIC、外部 PCI 中断控制器、SoC 中断控制器）、定时器（CLINT）→ 尽可能作为**驱动**由 discovery 发现；但 **CPU architected facilities**（per-CPU timer、CPU-local 中断机制）与 ISA/CPU 执行模型强相关，可以保留在 Arch / Core mechanism，不必一律降为驱动；
- 固件交接（OpenSBI / QEMU 经 a1 传入 DTB 指针；SBI 调用属于 arch 层）；
- CPU bring-up（arch 层 + discovery 提供的 CPU 信息）。

**Core 不应知道信息来自 FDT 还是 ACPI** —— boot 编排把各种 backend 归一化成 MachineInfo 后再交给 Core。

目录：

- `third_party/fdt`（git submodule，github.com/repnop/fdt）—— 当前 discovery backend 的解析器：纯库、no_std、零依赖，只解析字节格式
- `tests/fixtures/fdt/` —— discovery backend 的解析器**测试 fixture**（如 qemu-virt.dts；DTS 描述机器而非 ISA；运行时 DTB 由机器提供）

新板子 = 新的机器描述来源 + 对应驱动，不需要新 crate。

> **platform quirks**（未来）：少数无法用标准描述 / probing 表达的怪异硬件行为，
> 允许少量特例代码作为 escape hatch —— 但它是例外，不是默认架构。

### Arch 与 Machine Discovery 的关系

- Arch 提供 ISA 能力（含固件调用原语），Machine Discovery 提供机器数据，两者都位于 Core 之下；
- Core 不直接处理机器细节：bootstrap 阶段做 discovery → 归一化 MachineInfo → `core::init(&MachineInfo)`（单镜像内函数调用）；
- 换架构时改 Arch，换 discovery backend 时换机制，Core 不变 —— 这是多架构支持的根基。

依赖方向（当前模型，精确表述）：

```text
BUILD TIME：bootstrap(riscv64) ──► kernel/core（library）+ arch（ISA）＋ fdt
                     │
                     ▼ (链接)
              kaleidos.elf（单镜像）
   ┌──────────────────────────────────────────┐
   │ 阶段一 Bootstrap：FDT → MachineInfo       │
   │        ↓ core::init(&MachineInfo)        │
   │ 阶段二 Resource Core（消费 MachineInfo）   │
   └──────────────────────────────────────────┘

RUN TIME（未来：组件热插拔）：

OpenSBI → kaleidos.elf
  → bootstrap：discover（fdt/ACPI... backend）→ MachineInfo
  → core::init(MachineInfo)（校验 → 提交资源真相）
  → Component Manager 解包内嵌 .initpkg（cpio 归档）
  → 按 manifest（文本）加载组件 .kcomp（ELF，Linux insmod/depmod 模式）
```

- **Resource Core 不依赖具体 Arch 和 Discovery backend**（core-lib 不依赖 arch/fdt；bootstrap 阶段才用）；
- **Bootstrap 与 Core 职责分离、装载合一**——两者都只在启动时加载一次、永不热替换，所以链接成一个 `kaleidos.elf`（职责边界 ≠ 装载边界；单镜像 + 高半区问题由链接脚本两段 + 页表双映射解决，Linux 同款，见 roadmap）；
- **Component 才需要独立装载边界**（`.kcomp` = ELF 可重定位文件 + 符号表，Linux `.ko` 模式）；打包用 **cpio 归档**（`initramfs` 模式）而非自定义二进制格式；manifest 是纯文本（`modules.dep` 模式）；
- **Cargo 依赖图 ≠ Component 图**：Cargo 边是编译期构建关系，运行时组件组合由 Component Manager 决定——组件热替换是 KaleidOS 的核心目标，但只在组件层（bootstrap/core 不做）。

## 4. Resource Core

Core 是整个系统的**资源权威 / 参考监视器（Resource Authority / Reference Monitor）**。

### Core 持有（owns truth）

- Task 与 CPU 执行状态（Task 身份、状态、运行在哪个 CPU、上下文）
- 物理内存（Physical Memory）：帧真相 + **canonical 帧分配器作为 Core 机制**（静态帧池，不热卸载）
- 共享 Core heap（Core 与组件共用，无 per-component 记账）
- AddressSpace
- IRQ / Timer / MMIO / DMA
- 内核对象（Kernel Object）
- Handle / Authority（不可伪造的授权）
- ComponentId
- ResourceDomain（组件拥有什么资源）
- 基础同步机制
- Trace / invariant 支持

### Core 不包含（这些属于 Component）

- RR / CFS 调度算法
- Ext4 / FAT 文件系统格式
- VFS
- TCP/IP 协议栈
- VirtIO / NVMe 协议
- POSIX 进程语义、Signal、Socket 语义
- ELF loader
- Wasm runtime

### Core 的边界判断

> 如果一个完全错误的 Component 能通过某个 API 破坏其他 Component 或全局 invariant，
> 那么应该**缩小 API**，或者把**最终 authority 收回 Core**。

这条"litmus test"是新增内容进 Core 的唯一判据：进 Core 的不是"基础功能"，而是"撒谎就会全盘崩溃的真相"。

## 5. Component 与 Interface

### Component

- 消费（consume）和提供（provide）Interface；
- 拥有 ResourceDomain；
- 有生命周期；
- 可以包含子组件；
- 可以被替换 / 重启。

### Interface —— 组件提供什么能力

按领域分为三类：

| 类别 | 例子 |
|---|---|
| Device（设备） | BlockDevice、NetDevice、InputDevice、DisplayDevice、AudioDevice |
| Service（服务） | FileSystemService、NetworkService、GraphicsService、LoggerService、GameRuntimeService |
| Policy（策略） | SchedulerPolicy、PageReplacementPolicy（未来：MemoryPolicy） |

> **Interface 是语义，传输（transport）是绑定策略。**
> 第一阶段用 Rust trait + direct call；未来可以换成 IPC stub 或 Wasm host call。
> 因此接口描述**永远不要**绑定 native Rust ABI 细节。

### 授权流（Authority 流）

```text
Core
 │  grant Resource Authority（Handle）
 ▼
NVMe Component
 │  provides
 ▼
BlockDevice（Interface）
```

## 6. ResourceDomain 与 ExecutionDomain

每个 Component 概念上拥有两个域：

```text
Component
├── ResourceDomain   —— 它拥有什么（由 Core 记录）
└── ExecutionDomain  —— 它在哪运行
```

- **ResourceDomain**：组件持有的 Handle 集合（MmioHandle、IrqHandle、DmaHandle、TimerHandle...），由 Core 统一记录；它记录的是 **authority handle**（MMIO/IRQ/DMA/frame handle），**不是**堆字节数，也没有 per-component arena。组件停止时 Core 保证**最终回收**（graceful shutdown / forced containment 双路径，见 component-model.md §3，不预设 universal revoke order）；
- **ExecutionDomain**：第一阶段只需要 `KernelNative`（内核地址空间中的 Rust 函数）；未来可以有 `UserAddressSpace`、`WasmSandbox`。

> 架构上不要把 Component 永远绑定为"内核地址空间中的 Rust 函数"。契约（Interface + Handle）与执行域解耦，同一个组件图才能配置成宏内核、微内核或混合形态。

## 7. OS Profile

```text
OS = Resource Core + Component Graph + Profile
```

- Profile 描述一套完整的组件图（谁提供什么、谁依赖什么、各自执行域）；
- 预想 profile：`tiny`（RR 调度 + UART + RAMFS，全 native）、`game`、`unix`（POSIX personality）、`micro`（独立执行域 + IPC）、`wasm`、`debug`（调试分配器 + CoreTest + fault injection）；
- 第一个 profile：`minimal`。

同一份 Core，不同的 Profile = 完全不同的操作系统形态。这是 KaleidOS 区别于一般"模块化"架构的核心特点。

## 8. 关键数据流：Policy proposes, Core validates and commits

### 调度示例

```text
Scheduler（Component）         Core
    │                           │
    │  propose: 运行 Task #7    │
    │ ────────────────────────► │ 检查：Task #7 存在？
    │                           │ 检查：Runnable？
    │                           │ 检查：没在别的 CPU 上跑？
    │                           │
    │ ◄──────────────────────── │ commit / reject（记录 trace）
```

### 分配示例（Core 内部机制）

物理帧分配是 Core 内部机制，不是"提议 → 验证"的策略流：

```text
请求者（Component）            Core
    │                           │
    │  请求一帧                 │
    │ ────────────────────────► │ 分配器选择一帧（如 Frame #100）
    │                           │ 验证：存在？空闲？归属合法？
    │                           │ commit ownership
    │ ◄──────────────────────── │ grant authority（FrameHandle）
```

未来若引入 `MemoryPolicy` 组件，它只能提议偏好（NUMA 偏好、配额），最终选择/验证/提交仍在 Core。

**含义**：策略可以随便想、随便错；但任何对真实资源的改动，都必须经过 Core 验证并记录。Core 拒绝时留下 trace（policy proposal / Core rejection），这是调试和 CoreTest 的抓手。

## 9. 与参考系统的关系（详见 references.md）

- Asterinas → 策略注入与策略输出验证（propose/validate 先例）
- seL4 → typed authority、不可伪造 capability（Handle 设计来源）
- Exokernel → 保护与管理分离（Core/Component 分工的理论源头）
- Theseus / RedLeaf → 状态归属与资源回收（ResourceDomain 思想）
- Singularity / Inferno / Wasm → 执行域与虚拟 ISA（未来方向）

## 10. 本文件与其他文档的关系

- `core-philosophy.md`：为什么这样设计（判断标准与取舍）；
- `component-model.md`：组件的完整模型（生命周期、关系、替换）；
- `testing.md`：如何保证 Core 可信；
- `roadmap.md`：按里程碑怎么一步步长出来；
- `references.md`：每个参考系统借鉴什么、怎么用。