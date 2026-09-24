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
- **Bootstrap 与 Core 职责分离、装载合一**——两者都只在启动时加载一次、永不热替换，所以链接成一个 `kaleidos.elf`（职责边界 ≠ 装载边界；单镜像 + 高半区问题由链接脚本两段 + 页表双映射解决，Linux 同款，见 roadmap）；
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

### 内存模型：语义与机制分离

Core 的内存模型不等于某一种页表格式。内存相关概念保持三层分离：

```text
Physical Memory
    机器有哪些 RAM、哪些帧可分配、帧的 owner 是谁

Protection
    某个 Domain/Component 是否拥有访问某个区域的权利

Address Translation
    一个地址如何从 VA 翻译到 PA
```

Core 面向 `MemoryDomain`、region、ownership 和 permission；它不应该知道
`VPN`、`PTE`、`satp` 或某个具体页表遍历算法。`MemoryDomain` 表达执行实体
拥有哪些内存、允许访问哪些区域，以及这些区域的权限。

有 MMU 的平台可以由 `MemoryDomain` 关联 paged `AddressSpace`，由架构 backend
实现地址翻译和硬件权限；没有 MMU 的平台则可以使用 flat memory 加 PMP/MPU
等 protection backend。NoMMU 不是一种特殊的页表，也不承诺具备 page fault、
COW 或任意虚拟地址空间等 MMU 语义。

**MMU / NoMMU 是平台能力，不是哲学分叉。** Core 不因平台能力缺失而拒绝平台，而是
允许能力降级，并把差异统一到 `AddressSpaceManager` / `MappingSource` 抽象
（map / unmap / domain / protection），backend 可以是 RV64+Sv39、RV32+Sv32 或 NoMMU。
能力差异必须显式可见、绝不假装：

| 平台能力 | 可承诺的隔离 |
|---|---|
| MMU + IOMMU | 强隔离（CPU + DMA 均受控） |
| MMU，无 IOMMU | CPU 隔离；DMA 受限（需信任或额外约束） |
| NoMMU | 基于信任的 native domain，无硬件强制 |

每一处能力差异都必须在 Profile / 机器描述中显式呈现，而不是在 Core 里用同一套假设
抹平（差异如何暴露给驱动见 `driver-model.md`）。

当前 RISC-V 已提供 RV64/Sv39 与 RV32/Sv32 backend，但它们只是 address-translation
实现，不是 KaleidOS 的内存模型。Core 公共路径使用 virtual region、PhysicalRange、
抽象 permission 等概念；裸 `PhysAddr`、PTE、VPN、`satp` 和 TLB 操作留在
arch/backend 内部。具体映射、撤销和地址空间激活接口随实现阶段演进，
不在这里提前固定完整 API。

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

- Core Export ABI 是 **Component → Core 的 mechanism boundary**：导出内存获取
  （`kcore_memory_acquire/release`，域视图）、输出通道、已提交真相的只读查询，以及经过
  Core 处理的**语义入口**（组件加载 / endpoint 发布 / 任务控制 / 设备与 IRQ 与 DMA：
  `kcore_device_nth` + `kcore_device_claim/release`、
  `kcore_irq_register/enable/disable/release`、
  `kcore_dma_alloc/free/map/unmap`）。**不导出未经 Core 提交的裸 mutation**：
  物理帧分配的最终提交、地址空间变更、裸任务表改动仍是 Core 内部提交点——
  组件只能 request，验证 + commit + 记录（owner / trace）由 Core 完成。
- Component Endpoint Registry 是 **Core 的组件依赖真相**：谁在哪个端口发布了哪个
  契约、endpoint 是否存活。两者是独立概念，互不替代。
- 内存粒度定案：`ALLOC_GRANULE`（物理分配）与 `AddressSpaceBackend::GRANULE`
  （VM 映射）语义解耦；RISC-V trap 按特权级拆分（`trap/supervisor.rs` =
  S-mode 机制，`trap/machine.rs` = M-mode 骨架，共享解码在 `trap/mod.rs`），
  未来 S-mode+MMU 与 M-mode+NoMMU 双 profile 不互相牵制。

### Core ABI 错误约定（v3 起）

```text
0          success
-negative  failure: -Errno
```

- `Errno` 是稳定、Linux/POSIX 风格的数值命名空间（`os/core/src/errno.rs`）：
  **完整的 `asm-generic/errno` 集合**（1–133）。现成的 no_std errno crate 全部门控在
  hosted / Linux（`libc` 的常量在 `#[cfg(target_os = "linux")]` 之类的模块里，裸机
  取不到），所以编号由我们自己持有——但**数值照抄标准、不发明**。进入 public ABI 后
  数字不再变更。
- 组件面是同一套码：Rust 侧 `kcomp-sdk` 的 `Errno` / `Result<T>`（`src/errno.rs`），
  C 侧 `<errno.h>` shim（`include/errno.h`；`-ffreestanding` 不提供）。三方数值由
  `os/core/tests/kcomp_abi_drift.rs` 钉死。
- 各子系统的内部错误（`TaskError` / `ComponentLoadError` / `SchedError` /
  `EndpointError` / `CallError` / `DeviceClaimError` / `DeviceReleaseError` / `IrqError` /
  `DmaError` / `MemoryError` ...）保持丰富与类型安全，
  只在 Core ABI 边界翻译成 `Errno`——映射表集中在 `errno.rs`。
- 组件（Rust / C / Wasm / IPC）只需要理解这一套错误码。

**返回值形状**（按"能否失败"分类，无例外）：

| 形状 | 用于 | 例 |
|---|---|---|
| `i32 status`（`0` / `-Errno`） | 可失败、无值 | `kcore_task_yield` |
| `i32 status + out` | 可失败、有值 | `kcore_device_claim` |
| 直接返回值 | 不会失败的纯 query（`0` 是普通值，不是哨兵） | `kcore_free_page_count` |

**宽度规则**（kcore ABI 数值类型的唯一口径）：

| 宽度 | 用于 |
|---|---|
| `usize` | 仅"语义就是指针宽"的量：地址（`entry`）、`(ptr, len)`、分配器 `size/align` |
| `u32` | counts / ids（hart / cpu / page / task / component ...） |
| `i32` | 布尔与编码（`has_hart` / `task_state`） |
| `u64` | 不透明 id（`EndpointId` / DMA mapping id），只经 `status + out` 回传 |

KernelNative 下组件与 Core 同 target 编译，宽度天然一致；跨 transport（IPC / Wasm）
不复用本签名，宽度另行定义。旧 v1/v2 的 `id >= 0 / -Errno` 值型签名保持兼容；
新增"可为空的查询"用 `status + out`，不拿 0 当哨兵（`boot_hart` 的 0 是历史唯一样本）。

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

- **ResourceDomain**：组件拥有的资源**归属集合**，由 Core 统一记录。它只记设备所有权（claimed `DeviceId`）、IRQ route、DMA allocation/mapping，**不**记受管内存——Core 不做内存记账、无 region owner 记录，**不是**逐帧 identity、堆字节数，也没有 per-instance arena（per-instance `HeapState` 属于 runtime，不是 ResourceDomain 资源；见 `docs/architecture/memory-and-heap.md`）。**实现决策：不设 ResourceDomain struct** —— 它是一个"视图"（所有 `owner == ComponentId(id)` 的归属记录），owner 字段直接落在各资源表（device/irq/dma）的 record 上，回收 = `revoke_owner(id)`（见 component-model.md §3）。组件停止时 Core 保证**最终撤销归属并做 teardown/quarantine**（graceful shutdown / forced containment 双路径，不预设 universal revoke order）；
- **ExecutionDomain**：实现形态是 owning enum —— `KernelNative` / `IsolatedNative(AddressSpaceId)`（未来可加 `SandboxedNative`）。**它只回答"在哪运行、什么特权 / 地址空间"**；执行模型 / ISA / runtime（native machine code vs Wasm）是**正交维度**，不属于这里——Wasm 是未来 Component 的一种执行后端，不是第四个执行域（见 `deployment.md` §3）。**现状**：image 与 instance 已分离（`ComponentImageId` + `InstanceRecord`，见 `docs/architecture/component-lifecycle.md`），旧 `ComponentRecord` 已删除；实例记录已带 `execution_domain` 字段（`InstanceRecord`），由创建入口验证后写入；`KernelNative` 与受限的 `IsolatedNative`（私有 AS + assembly gateway 生命周期 + KernelNative → Isolated 跨域 service Gate；无 import 面、无出站 Isolated 调用，见 `deployment.md` §6/§10）都有真实执行器，`SandboxedNative` 是 `todo!()` 占位。`ComponentRuntime`/`ComponentManager` 仍是目标，未见代码。ExecutionDomain 只引用 AddressSpace 身份，不拥有可独立修改的页表对象。
  - **D2=A 定位**：`KernelNative`（S + 共享内核 AS）是常态、长期模式，**KernelNative 就是可信代码**（无硬件访问强制，撤销为协作式）；`IsolatedNative`（S + 私有 AS）是可选教学实验、**非里程碑**，只做条件性故障隔离；`SandboxedNative`（U + 私有 AS）才是未来的硬件强制边界。驱动 / device claim / IRQ / DMA / teardown 不变式见 `driver-model.md`。

**三个组件信任域（Trust Domain）与 ABI 分离：**

| 域 | 特权级 | 地址空间 | 信任假设 | 边界 ABI |
|---|---|---|---|---|
| KernelNative | S-mode | 共享内核 AS | 完全可信 | 窄稳定 `extern "C"` C ABI |
| IsolatedNative | S-mode | 私有 AS | 半可信 | 受控边界 |
| SandboxedNative | U-mode | 私有 AS | 不可信 | syscall wire 格式 |

- 同一语义操作（allocate / map / irq / log / interface-call）在三个域中是同一件事，
  但 **transport / ABI 必须分离**：不能因为"做的是同一件事"就强推同一套底层 ABI；
- 因此 KaleidOS **既不"必须是微内核"、也不"必须是宏内核"**：不同信任级使用不同边界，
  同一组件图按需组合成宏内核、微内核或混合形态。

**部署形态本身就是安全策略，Core 不统一强制。** 若不信任一个组件，就不要把它部署成
`KernelNative`——而不是让 Core 把每个组件都塞进同等重量的机制。信任问题首先由"选择
哪种执行域"回答，Core 不为统一性牺牲部署自由度。

**AddressSpace 是 Core 内部机制，不是驱动可取用的对象。** 驱动不应取得任意地址空间后到处映射；设备窗口的映射由 `kcore_device_claim` 在解析调用者 execution domain 时自动完成（KernelNative identity，未来 Isolated 映射进组件 AS）。Core 只在 `driver-model.md` §3 的同一 seam 内决定返回裸指针还是 mapped VA：

```text
driver   claim(device_id)
           │
           ▼
Core     解析调用者的 execution domain（推导目标 AS）
           │
           ▼
Core     记 device owner + 解析本域窗口（KernelNative 裸指针 / Isolated mapped VA）
           │
           ▼
Core     记录 ownership / trace
```

> 架构上不要把 Component 永远绑定为"内核地址空间中的 Rust 函数"：契约（Interface + mechanism）与执行域解耦，才能在同一组件图上自由选择信任边界。

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
    │  请求一段内存区域         │
    │ ────────────────────────► │ 分配器选择可用 PhysicalRange
    │                           │ 验证：尺寸合法？空闲？范围合法？
    │                           │ commit：占用该块并交付 backing（不记 owner）
    │ ◄──────────────────────── │ 返回本执行域访问窗口（kcore_memory_view）
```

Core **不做内存记账**：不记 owner / 不发 region id / 无 Retired 表；KernelNative 无隔离，Isolated 的归属由该实例的 AS / 页表承载（`MemoryLease` 只是 Core 内部 RAII，不对外暴露）。契约见 `docs/architecture/memory-and-heap.md`。

未来若引入 `MemoryPolicy` 组件，它只能提议偏好（NUMA 偏好、配额），最终选择/验证/提交仍在 Core。

**含义**：策略可以随便想、随便错；但任何对真实资源的改动，都必须经过 Core 验证并记录。Core 拒绝时留下 trace（policy proposal / Core rejection），这是调试和 CoreTest 的抓手。

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
- `roadmap.md`：按里程碑怎么一步步长出来；
- `references.md`：每个参考系统借鉴什么、怎么用。
