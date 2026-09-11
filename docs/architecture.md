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
  → （未来）Component Manager 解包内嵌 .initpkg（cpio 归档）
  → （未来）按 manifest（文本）加载组件 .kcomp（ELF，Linux insmod/depmod 模式）
```

- **Resource Core 不依赖具体 ISA 实现和 Discovery backend**（core-lib 只依赖 `os/arch` 的稳定 contract，不依赖 fdt；bootstrap 负责组合具体实现）；
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

当前 RISC-V 已提供 RV64/Sv39 与 RV32/Sv32 backend，但它们只是 address-translation
实现，不是 KaleidOS 的内存模型。Core 公共路径使用 typed handle、virtual region、
permission 等抽象；裸 `PhysAddr`、PTE、VPN、`satp` 和 TLB 操作留在
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

### 两条机制边界（Core ABI ≠ Interface Registry）

```text
Component → Core          = Core Export ABI（export.rs：kcore_* 白名单，
                            稳定 C ABI、exact-name resolution、未导出 → UnresolvedSymbol）
Component → Component     = Interface binding（interface.rs：publish/resolve/unbind，
                            逻辑 binding + versioned vtable，禁止 flat ELF symbol 互链）
```

- Core Export ABI 是 **Component → Core 的 mechanism boundary**：导出共享堆
  （`kcore_heap_alloc/dealloc`）、输出通道、已提交真相的只读查询，以及经过
  Core validation 的**语义入口**（组件加载 / 接口发布 / 任务控制 / 资源
  claim：`kcore_mmio_claim/read`）。**不导出未经 Core validation 的裸
  authority mutation**：物理帧分配的最终提交、地址空间变更、裸任务表改动
  仍是 Core 内部提交点——组件只能 request（propose），authorize + grant +
  记录由 Core 完成。
- Component Interface Registry 是 **Core 的组件依赖真相**：谁提供什么接口、
  当前绑到谁。两者是独立概念，互不替代。
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
  用到哪个加哪个，进入 public ABI 后数字不再变更。
- 各子系统的内部错误（`TaskError` / `HandleError` / `ComponentLoadError` /
  `SchedError` / `InterfaceError` / `MmioError` ...）保持丰富与类型安全，
  只在 Core ABI 边界翻译成 `Errno`——映射表集中在 `errno.rs`。
- 组件（Rust / C / Wasm / IPC）只需要理解这一套错误码。

**返回值形状**（按"能否失败"分类，无例外）：

| 形状 | 用于 | 例 |
|---|---|---|
| `i32 status`（`0` / `-Errno`） | 可失败、无值 | `kcore_task_yield` |
| `i32 status + out` | 可失败、有值 | `kcore_mmio_read_u32` |
| 直接返回值 | 不会失败的纯 query（`0` 是普通值，不是哨兵） | `kcore_free_page_count` |

**宽度规则**（kcore ABI 数值类型的唯一口径）：

| 宽度 | 用于 |
|---|---|
| `usize` | 仅"语义就是指针宽"的量：地址（`entry`）、`(ptr, len)`、分配器 `size/align` |
| `u32` | counts / ids（hart / cpu / page / task / component ...） |
| `i32` | 布尔与编码（`has_hart` / `task_state` / `interface_available`） |
| `u64` | 不透明句柄，只经 `status + out` 回传 |

KernelNative 下组件与 Core 同 target 编译，宽度天然一致；跨 transport（IPC / Wasm）
不复用本签名，宽度另行定义。旧 v1/v2 的 `id >= 0 / -Errno` 值型签名保持兼容；
新增"可为空的查询"用 `status + out`，不拿 0 当哨兵（`boot_hart` 的 0 是历史唯一样本）。

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

- **ResourceDomain**：组件持有的 Handle 集合（MmioHandle、IrqHandle、DmaHandle、TimerHandle...），由 Core 统一记录；它记录的是设备/执行域 authority 和受管理的内存区域，**不是**逐帧 handle、堆字节数，也没有 per-component arena。**实现决策：不设 ResourceDomain struct** —— 它是一个"视图"（所有 `owner == ComponentId(id)` 的资源），owner 字段直接落在各资源表（irq/mmio/dma/timer）的 record 上，回收 = `revoke_owner(id)`（见 component-model.md §3）。组件停止时 Core 保证**最终回收**（graceful shutdown / forced containment 双路径，不预设 universal revoke order）；
- **ExecutionDomain**：实现形态是 owning enum —— `KernelNative` / `IsolatedNative(AddressSpaceId)`（未来可加 `Wasm`）。`ComponentRecord` 只记轻量 `execution_kind`，真正的 runtime（`LoadedComponent` + `ExecutionDomain`）放 `ComponentRuntime`，由 `ComponentManager` 串起来（见 component-model.md §4）。ExecutionDomain 只引用 AddressSpace 身份，不拥有可独立修改的页表对象。

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
    │  请求一段内存区域         │
    │ ────────────────────────► │ 分配器选择可用 PhysicalRange
    │                           │ 验证：存在？空闲？范围合法？
    │                           │ commit region ownership
    │ ◄──────────────────────── │ 返回受 Core 管理的 memory lease
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
