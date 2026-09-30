# KaleidOS 架构总览（architecture.md）

## 1. 项目定位

KaleidOS 是一个**组件化、多架构**的操作系统，面向学习、实验与个人创作。

- 它不是 Linux 的复刻，POSIX / BusyBox 兼容只是未来可选的一种 *Profile*；
- 核心目标是亲手实现 OS 的关键基础机制，并让 Core 以上的大部分 OS 功能成为**可组合的 Component**；
- 同一个 Resource Core 底座，通过重新组合 Component，可以长出完全不同的操作系统（One core, countless systems）。

> **A small mechanism-first core beneath a composable graph of operating-system components.**
> 中文：一个提供机制与所有权记账的小型 Core，以及构建在其上的可组合操作系统组件图。

## 2. 分层结构

```text
Applications / System Personality（应用 / 系统性格）
                │
         Services / Devices（服务 / 设备）
                │
            Components（组件）
                │
          Resource Core（机制 + 所有权核心）
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
Identity         ≠  Ownership           —— 名字 ≠ 所有权（DeviceId 不是权限）
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
- CPU-local firmware-call boundary（如 RISC-V SBI）可以由对应 backend 提供；**UEFI runtime 调用属于 Boot/Firmware environment，不是 ISA 属性**，不归 x86 arch 所有

目录：`os/arch`（统一 crate：`CpuArch`、`Console`、`SystemReset` backend trait + cfg 选择——host 编译用 fake 实现，交叉编译用 `riscv` ISA family 实现）。后续 ISA：`x86_64` / `aarch64` / `loongarch64`（各自模块 + cfg 分支）。

当前先拆出窄的 backend contract，不提前建立独立 Platform crate：RISC-V 的
SBI 调用集中在 `riscv/firmware.rs`，CPU 原语在 `riscv/cpu.rs`，陷阱和上下文
分别位于 `riscv/trap/` 与 `riscv/context/`；XLEN-specific 汇编使用
`trap64.S`/`switch64.S` 这类窄变体。同一 ISA 支持多个板卡时再由 boot profile

**boot 期内核页表策略不归 arch 所有**：identity + high-half 双映射、段权限、
临时 root、high-half hand-off 属于 KaleidOS boot policy，位于 boot crate 的
`vm/`（`layout.rs` = linker symbols 唯一解释者，`bootstrap.rs` = 临时静态页表，
`runtime.rs` = 长期 buddy 页表骨架）。arch 的 `riscv/mmu` 只保留 Sv39/Sv32
翻译机制（页表编码/walk、`activate`、`flush_tlb`），不知道 `KERNEL_VMA`、
`.text`、`.initpkg` 或 bootstrap hand-off。
选择对应的 Console/SystemReset backend。

### 责任边界：Arch / Platform（职责，不是新层）/ Boot

`Arch`、`Platform`、`Boot` 是**职责划分**，不是三个 crate，更不是一个新的分层：

| 责任 | 内容 | 代码位置 |
|---|---|---|
| **Arch** | CPU / ISA 原语：trap 与分发、上下文切换、MMU/TLB、中断开关、原子原语、CPU-local idle（`wait_for_interrupt` / `atomic_idle`）、CPU architected 定时器与 CPU-local firmware boundary（SBI 等） | `os/arch/` |
| **Platform（职责，不是 crate / 层）** | 机器发现与接线：把 FDT / ACPI / probing 归一化为 `MachineInfo`，处理板级拓扑、发现到的控制器 / 设备接线 | 由 boot crate 内的 discovery 承担；**不**新立 Platform crate |
| **Boot** | loader / firmware / boot protocol handoff：入口汇编、启动期页表、把 `MachineInfo` 与 reserved 交给 `core::init` | `os/boot/<arch>/` |

不变式：Arch 不认识板卡名（不能有 `if board == ...` 的机制分叉）；Machine Discovery 不定义 ISA 契约；Core 不知道事实来自 FDT 还是 ACPI。三者只通过稳定 backend trait 与 `MachineInfo` 相接。

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

- Arch 提供 ISA 能力，Console/SystemReset 等固件服务由 backend adapter 提供；Machine Discovery 提供机器数据，两者都位于 Core 之下；
- Core 不直接处理机器细节：bootstrap 阶段做 discovery → 归一化 MachineInfo → `core::init(&MachineInfo)`（单镜像内函数调用）；
- 换架构时改 Arch，换 discovery backend 时换机制，Core 不变 —— 这是多架构支持的根基。

### 从 Zephyr 借鉴的硬件边界（不复制 platform 层）

Zephyr 的 `arch / SoC / board / device driver` 拆分值得保留为**概念模型**：

```text
Architecture   = ISA / 特权级 / trap / context / MMU 等机制
SoC            = 芯片级中断、clock、memory map 等能力
Board          = 具体 PCB 的内存、外设实例和连接关系
Driver         = 设备协议实现，通过 generic Interface 提供语义
```

在 KaleidOS 中，前三者不要求一一对应三个 crate：

- `os/arch` 只承载 ISA 与 CPU 原语；不能出现 `if board == ...` 才改变的架构机制；
- SoC/board 的事实由 Machine Discovery backend 发现，归一化为 `MachineInfo` /
  `DeviceDescriptor` 后交给 Core；Core 不知道事实来自 FDT、ACPI 还是其他来源；
- driver 是 `os/components/drivers/` 下的 Component，用 `kcore_device_nth` 发现候选、
  `DeviceId` 认领确切设备并拿到本执行域访问窗口（KernelNative = 裸寄存器基址），再发布 Device Interface；
- 当前不建立独立 `platform` 层，是为了避免把板卡目录结构误当成 Core 契约。未来若
  平台特例增多，只能加入窄的 discovery/backend 模块，不能让 board 名称渗透资源模型。

因此，硬件链路固定为：

```text
FDT / ACPI / probing
        ↓
Machine Discovery → MachineInfo（事实提案）
        ↓ core::init 校验并提交
Core Resource Truth
        ↓ DeviceId（身份，不是权限）
driver claim → 本执行域访问窗口（裸 MMIO 指针 / 映射 VA）
        ↓
Component Interface（设备语义）
```

另一个直接借鉴是**按能力而不是按板卡名选择机制**。`.config` / Kconfig 表达
build/profile 能力，`MachineInfo` 表达运行时机器事实；MMU/NoMMU、特权级、PMP/MPU、
IOMMU/DMA isolation 等能力必须按轴表达。Core 与执行域只承诺实际能力支持的强制
程度，详见 `driver-model.md` §11。未来如果需要 `ArchCapabilities`，它应是窄的
能力契约，而不是包含所有板卡差异的大枚举。

依赖方向（当前模型，精确表述）：

```text
BUILD TIME：os/boot/riscv（bin）──► os/core（library）+ os/arch（backend traits）＋ fdt
                     │
                     ▼ (链接)
              kaleidos.elf（单镜像）
   ┌──────────────────────────────────────────┐
   │ 阶段一 Boot：FDT → MachineInfo            │
   │        ↓ core::init(&MachineInfo)        │
   │ 阶段二 Resource Core（消费 MachineInfo）   │
   │        ↓ monitor::run()                  │
   │ 阶段三 Core Monitor（core> 交互 shell）    │
   └──────────────────────────────────────────┘

RUN TIME（未来：组件热插拔）：

OpenSBI → kaleidos.elf
  → boot：discover（fdt/ACPI... backend）→ MachineInfo
  → core::init(MachineInfo)（BSS 清零 → MetadataHeap 帧分配器 → 探测）
  → Core Monitor（core> 交互调试面，裸 Core 常态能力）
  → Component Manager 解包内嵌 .initpkg（cpio 归档）→ store → loader → registry（已落地）
  → 按 manifest（文本）装载组件 .kcomp（ELF，Linux insmod/depmod 模式；已落地）
  → （未来）运行期热插拔 / Runtime Graph / 依赖解析
```

- **Resource Core 不依赖具体 ISA 实现和 Discovery backend**（core-lib 只依赖 `os/arch` 的稳定 contract，不依赖 fdt；bootstrap 负责组合具体实现）；
- **Bootstrap 与 Core 职责分离、装载合一**——两者都只在启动时加载一次、永不热替换，所以链接成一个 `kaleidos.elf`（职责边界 ≠ 装载边界；单镜像 + 高半区问题由链接脚本两段 + 页表双映射解决，Linux 同款，见 `STATUS.md`）；
- **Component 才需要独立装载边界**（`.kcomp` = ELF 可重定位文件 + 符号表，Linux `.ko` 模式）；打包用 **cpio 归档**（`initramfs` 模式）而非自定义二进制格式；manifest 是纯文本（`modules.dep` 模式）；
- **Cargo 依赖图 ≠ Component 图**：Cargo 边是编译期构建关系，运行时组件组合由 Component Manager 决定——组件热替换是 KaleidOS 的核心目标，但只在组件层（bootstrap/core 不做）。

## 4. Resource Core

Core 是整个系统的**机制与所有权真相核心（mechanism & ownership core）**：提供推进自身资源/生命周期操作所需的机制，并记录裸机程序无法自行知道的所有权真相。**它不是 capability 系统，也不对 KernelNative 做访问强制**（见 `driver-model.md` §1.1）。

### Core 持有（owns truth）

- Task 与 CPU 执行状态（Task 身份、状态、运行在哪个 CPU、上下文）
- 物理内存（Physical Memory）：帧真相 + **canonical 帧分配器作为 Core 机制**（静态帧池，不热卸载）
- Core 对象堆（仅 Core 内部；组件不共享）与 **region 粒度**的 backing / mapping（Core **不**做内存记账：不记 region owner，无隔离域不记归属，Isolated / Sandboxed 由该实例的 AS / 页表承载；见 `docs/architecture/memory-and-heap.md`）
- AddressSpace
- IRQ / Timer / MMIO / DMA
- 内核对象（Kernel Object）
- 设备所有权 / IRQ route / DMA mapping 的**归属记账**（用于独占、unload、失败清理、teardown、quarantine；**不是** per-access 鉴权）
- ComponentId
- ResourceDomain（**视图**，非对象：所有 `owner == ComponentId` 的资源；**不设 struct**，见 `docs/architecture/component-model.md` §3）
- 基础同步机制
- Trace / invariant 支持

这里的 AddressSpace 只表示 Core 管理的资源真相；当前 M0.5 的启动页表是
Arch 的一次性机制，不是运行期 AddressSpace。未来运行期地址空间的语义映射由
Core 独占提交，Sv39/Sv32 页表只是 Arch backend 维护的硬件投影。Arch 可以保存这份
投影，但不能绕过 Core 独立改变映射、所有权或生命周期。

### 内存模型

Core 的内存模型不等于某一种页表格式，保持三层分离：Physical Memory（谁拥有哪些帧）/ Protection（谁能访问）/ Address Translation（VA→PA）。Core 面向 region / address-space 语义；`VPN` / `PTE` / `satp` 与具体 walk 算法留在 arch backend。

**MMU / NoMMU 是平台能力，不是哲学分叉**：Core 允许能力降级并把差异统一到 `AddressSpaceManager` / backend，能力差异必须显式可见、绝不假装：

| 平台能力 | 可承诺的隔离 |
|---|---|
| MMU + IOMMU | 强隔离（CPU + DMA 均受控） |
| MMU，无 IOMMU | CPU 隔离；DMA 受限（需信任或额外约束） |
| NoMMU | 基于信任的 native domain，无硬件强制 |

内存 / 堆分层、无账本与访问窗口契约见 `docs/architecture/memory-and-heap.md`；能力如何暴露给驱动见 `docs/architecture/driver-model.md`。

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
> 那么应该**缩小 API**，或者把**最终裁决权收回 Core**。保留一个 Core API 的判据：是否**只有 Core 能**操作页表 / 知道全局设备所有权 / 路由 IRQ / 管理组件生命周期 / 避免 DMA backing 被错误复用；否则删除。

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

**Rust ABI 不得成为 Component ABI。** 边界处禁止出现：Rust mangled symbol、
Rust trait-object ABI、`fmt::Arguments`、`PanicInfo`、allocator 内部结构、
Rust enum layout、编译器私有 runtime 结构。KernelNative 边界使用窄而稳定的
C ABI：`extern "C"`、定宽整数、pointer + length、显式 status code、opaque handle、
explicit-layout struct（Core Export ABI 即此形状）；U-mode 另行定义自己的 syscall wire ABI。

### 两条机制边界（Core ABI ≠ Endpoint Registry）

```text
Component → Core          = Core Export ABI（export.rs：kcore_* 白名单，
                            稳定 C ABI、exact-name resolution、未导出 → UnresolvedSymbol）
Component → Component     = Endpoint binding（endpoint.rs：publish/lookup/discover/bind，
                            opaque EndpointId + exact ABI fingerprint + typed #[repr(C)]
                            function table 或 Core call gate，禁止 flat ELF symbol 互链）
```

Core Export ABI 不导出**未经 Core 提交的裸 mutation**：物理帧分配的最终提交、地址空间变更、裸任务表改动都是 Core 内部提交点——组件只能 request，验证 + commit + 记录（owner / trace）由 Core 完成。Endpoint Registry 是 Core 的组件依赖真相；两者是独立概念，互不替代。

### Core ABI 错误约定与宽度

```text
0          success
-negative  failure: -Errno
```

`Errno` 是稳定、Linux/POSIX 风格的数值命名空间（完整 `asm-generic/errno`，1–133；数值照抄标准、不发明）；各子系统的内部错误只在 Core ABI 边界翻译成它（映射集中在 `os/core/src/errno.rs`），Rust / C 组件面是同一套码，三方数值由 `os/core/tests/kcomp_abi_drift.rs` 钉死。

- **返回值形状**（按"能否失败"分类）：`i32 status`（可失败、无值）/ `i32 status + out`（可失败、有值）/ 直接返回值（不会失败的纯 query）。
- **宽度规则**：`usize` 仅指针宽的量（地址、`(ptr,len)`、`size/align`）；`u32` counts / ids；`i32` 布尔与编码；`u64` 不透明 id（只经 `status + out` 回传）。KernelNative 同 target 编译天然一致；跨 transport 另行定义。

ABI 形状的单一来源是 `abi/*.toml`，生成物见 `docs/modules/core/generated.md`。

### 资源认领流（claim 流）

```text
Core
 │  mechanism：device_nth → device_claim（记 owner + 返回本域访问窗口）
 │             IRQ route / DMA mapping 归属记账
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

- **ResourceDomain**：组件拥有的资源**归属集合**，由 Core 统一记录。它只记设备所有权（claimed `DeviceId`）、IRQ route、DMA allocation/mapping，**不**记受管内存（Core 不做内存记账；堆是 runtime / deployment 策略，不是 ResourceDomain 资源）。**不设 struct**：它是"所有 `owner == ComponentId` 的归属记录"这一**视图**，owner 字段落在各资源表（device/irq/dma）的 record 上，回收 = `revoke_owner(id)`。组件停止时 Core 保证最终撤销归属并 teardown / quarantine。
- **ExecutionDomain**：回答"在哪运行、什么特权 / 地址空间"：`KernelNative` / `IsolatedNative(AddressSpaceId)`（未来可加 `SandboxedNative`）。执行模型 / ISA / runtime（native vs Wasm）是**正交维度**，不属于这里（Wasm 是未来 Component 的一种执行后端，不是第四个执行域）。一个 `ComponentId` = 一个完整运行组件（`ComponentRecord` 直接拥有自己的 `LoadedComponent`），其 `execution_domain` 由创建入口验证后写入；`KernelNative` 与受限 `IsolatedNative` 都有真实执行器，`SandboxedNative` 是 `todo!()` 占位。

**三个组件信任域与 ABI 分离：**

| 域 | 特权级 | 地址空间 | 信任假设 | 边界 ABI |
|---|---|---|---|---|
| KernelNative | S-mode | 共享内核 AS | 完全可信 | 窄稳定 `extern "C"` C ABI |
| IsolatedNative | S-mode | 私有 AS | 半可信 | 受控边界 |
| SandboxedNative | U-mode | 私有 AS | 不可信 | syscall wire 格式 |

同一语义操作在不同域是同一件事，但 **transport / ABI 必须分离**；**部署形态本身就是安全策略**，Core 不统一强制——不信任一个组件就不要把它部署成 `KernelNative`。AddressSpace 是 Core 内部机制，设备窗口的映射由 `kcore_device_claim` 在解析调用者 execution domain 时自动完成，不交给驱动随意映射。

> 驱动 / device claim / IRQ / DMA / teardown 与执行域细节分别见 `docs/architecture/driver-model.md` 与 `docs/architecture/deployment.md`。

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
    │  propose: 运行 Task #7    │
    │ ────────────────────────► │ 校验：存在？Runnable？未在别 CPU 跑？
    │ ◄──────────────────────── │ commit / reject（记录 trace）
```

### 分配示例（Core 内部机制）

物理帧分配是 Core **内部机制**，不是"提议 → 验证"的策略流：Core 选择并提交 backing，返回本执行域访问窗口，**不记 owner**。未来若引入 `MemoryPolicy` 组件，它只能提议偏好（NUMA 偏好、配额），最终选择 / 验证 / 提交仍在 Core。

**含义**：策略可以随便想、随便错；但任何对真实资源的改动都必须经 Core 验证并记录，拒绝时留下 trace（policy proposal / Core rejection）。契约细节见 `docs/philosophy/core-philosophy.md` 与 `docs/architecture/memory-and-heap.md`。

## 9. 与参考系统的关系（详见 references.md）

- Asterinas → 策略注入与策略输出验证（propose/validate 先例）
- seL4 → typed capability 与"资源真相在核心"（KaleidOS 只借用思想，**不实现 capability 系统**；access enforcement 交给执行域）
- Exokernel → 保护与管理分离（Core/Component 分工的理论源头）
- Theseus / RedLeaf → 状态归属与资源回收（ResourceDomain 思想）
- Zephyr → arch / SoC / board / device model 的硬件边界（映射到 Machine Discovery 与驱动 Component）
- Singularity / Inferno / Wasm → 执行域与虚拟 ISA（未来方向）

## 10. 本文件与其他文档的关系

- `core-philosophy.md`：为什么这样设计（判断标准与取舍）；
- `component-model.md`：组件的完整模型（生命周期、关系、替换）；
- `driver-model.md`：驱动 / device claim / 执行域 / teardown 安全的设计契约；
- `testing.md`：如何保证 Core 可信；
- `STATUS.md`（仓库根）：状态与计划（里程碑、依赖链、路线图）；
- `references.md`：每个参考系统借鉴什么、怎么用。
