# 组件模型（component-model.md）

> 概念页。身份/生命周期以 [组件生命周期](component-lifecycle.md) 为准，部署/binding
> 以 [部署契约](deployment.md) 为准，服务执行/组合以 [服务执行](service-execution.md)
> 为准。目标组合/恢复结构不表示当前已实现。

## 1. Component 是什么

Component 是 KaleidOS 的基本构建单元。它不只是"一个模块"，而是 **lifecycle 与 ownership 的同一单位（unit of lifecycle AND ownership）**：一个组件实例代表它的 code、execution、资源归属、interfaces、lifetime 与 failure state。因此它是一个完整的可管理单元：

- 消费（requires）和提供（provides）Interface；
- 拥有 ResourceDomain（Core 维护的资源集合）；
- 有生命周期（见 §5）；
- 可以依赖其他 Component；
- 可以包含子 Component（Composite，见 §7）；
- 可以被替换 / 重启 / 恢复。

一个实例因此可以拥有：tasks、stacks、claimed 设备、IRQ routes、DMA allocations/mappings、memory mappings、interfaces；失败时 Core 撤销可见资源归属并禁止后续调度，已发布 backing 保留驻留。逻辑失效不等于完整拆除（见 §3.3 与 §4.9）。

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
requires:  DeviceId（identity）+ Core mechanism（device_claim / irq_register / dma_*）
provides:  BlockDevice（Interface）

Ext4 Component
requires:  BlockDevice, PageCache
provides:  FileSystem

VFS Component
requires:  FileSystem providers, PageCache
provides:  FileSystemService
```

> Interface 是语义，传输是绑定机制。组件边界使用窄 C ABI；SDK 的 Rust trait 是组件内部前端。
> 接口定义方法、完成/等待、并发、借用与失效语义；Direct/Gate 窗口服从部署契约。
> Server/Worker 是独立执行模型，见 [服务执行](service-execution.md)。

### 2.1 绑定机制定案：Endpoint Registry（唯一绑定真相）

```text
Component → Core          = Core Export ABI（export.rs，ELF undefined symbol 白名单）
Component → Component     = Endpoint binding（endpoint.rs）——禁止 flat ELF symbol 互链
```

- 已加载组件的 exported ELF symbols **不组成全局符号表**：KaleidOS Component 是
  replaceable 的，直接 relocation 到 provider 函数地址会让替换非常困难。
- consumer 拿到的是 **opaque `EndpointId`**（组合期 `lookup` / `discover` 交付），
  不是"永不变更的 provider ELF 符号地址"。`bind` 时 Core 按两端执行域一次性选定
  机制：同域 Direct 交付 provider 的 `api` / `ctx`（`#[repr(C)]` function table +
  opaque state），跨域 Gate 只给 call-gate handle。endpoint **永不重定向**：
  provider 停止 / 失败 → 它的全部 endpoint 永久失效；新实例发布新 endpoint。
  已交付 Direct 表不能追回，publication 失效不等于所有业务会话已清理。
- **exact ABI fingerprint（`InterfaceAbi`，`#[repr(transparent)] u64`）取代
  version**：它没有版本兼容语义，只回答"provider 与 consumer 是否由完全相同的
  Service ABI contract 编译"。不一致必须拒绝 publish / validate / bind，绝不能把布局
  不同的 function table 交给 consumer。指纹由 `abi/*.toml` 生成并协调替换，
  不提供 compatible range 或陈旧 ABI 兼容别名。
- Core 真相：`EndpointRegistry` 记录 谁在哪个端口发布了哪个契约（ContractId /
  EndpointId / kind / abi / provider / port / api / ctx）；`publish` 在
  `kcomp_instance_create()` 期间只记录 pending（**staged**），create 成功后 Core
  原子提交；consumer `bind` 时 Core 再次校验 contract + abi + 存活（组件卸载/失败后
  endpoint 立即永久失效）。
- 阶段一 KernelNative 用 direct call / function table；传输升级（IPC / Wasm host
  call）不改 endpoint 数据模型。

### 2.2 `.kcomp` = 链接后的组件程序

`.kcomp` 不是 rustc 的 `.o`，而是**链接后的组件程序（linked component program）**。运行时动态加载保持不变，但构建管线是：

```text
component wrapper
  + third-party crates
  + Component SDK / CRT
  + reachable Rust support
      → staticlib / archive
      → selective extraction   （只抽真正可达的成员）
      → section GC             （丢弃未引用 section）
      → strip                  （去符号 / 元数据）
      → partial link
      → .kcomp
```

`.kcomp` 内部的符号边界是契约：

| | 内容 |
|---|---|
| DEFINED | component code、third-party crate code、必需的 Rust support、private helpers、`kcomp_instance_create` / `kcomp_instance_destroy` |
| UNDEFINED | 只允许显式放行的 `kcore_*` imports（对齐 §2.1 的 export 白名单） |

- **third-party crate 是组件私有实现**：`smoltcp`、`virtio-drivers`、buddy allocator helper、协议 parser 一旦被组件使用，就成为 `.kcomp` 内部细节。Core 不认识 `smoltcp::socket::udp::...`，也不导出任何 Rust compiler/runtime 符号去满足组件；Core 只暴露固定 ABI（内存 `kcore_memory_acquire/release`（域视图，见 `docs/architecture/memory-and-heap.md`）、`kcore_log_line`、`kcore_device_*`、`kcore_irq_*`、`kcore_dma_*`、`kcore_task_*`、`kcore_panic_escape` ...）。
- **不建 shared Rust runtime**：不为所有 `.kcomp` 提供"shared core crate / shared alloc / shared fmt blob / shared runtime / component runtime symbol bag"去动态链接——那会把 rustc 版本、compiler 实现细节、monomorphization、内部 ABI 与 runtime state 变成系统 ABI。第一步接受每个组件**私有携带**它确实需要的少量 Rust support，再用 archive extraction / section GC / strip 压到最小；只有真实测量之后、且只针对极少数稳定能力，才允许提升进 Core ABI。
- **loader 不是 Rust dynamic linker**：它只做段放置 + 对白名单 `kcore_*` 的重定位，不理解 Rust 内部 ABI。

> 上述管线已落地，且已拆成「语言前端 + 语言无关 packer」两段：
> Rust 前端 `tools/build-kcomp.sh` 编出 `staticlib`（SDK / 依赖随镜像私有携带），
> C 前端 `tools/build-kcomp-c.sh` 编出 freestanding `.o`（clang，不链 libc），
> 两者都把输入交给 `tools/kcomp-link.sh` 做 partial link + section GC + strip，产出
> ET_REL `.kcomp`——因此 `.kcomp` 是**语言无关的组件二进制格式**，不是 Rust 格式。
> 系统包与显式 host fixture 均经 `scripts/build/package.py` 调用这些脚本；packer 在输出前
> 校验「ET_REL + `kcomp_instance_create` / `kcomp_instance_destroy` DEFINED + UNDEF 只有 `kcore_*` + 无 loader 不支持的重定位」。
> 组件通过共用 `kcomp-sdk`（§2.3）使用 ABI / 入口 / 日志 / panic adapter。
> 两条语言路径消费**同一个 `kcomp.h`**：C 组件只有这一份声明 + SDK 的 C 运行时
> （`os/components/kcomp-sdk/c/kcomp_rt.c` 的 weak `mem*` / `strlen` / `strchr`，随组件
> 私有携带），直接调 `kcore_*`；Rust 组件在同一份声明上加 SDK 的 Rust adapter（入口宏 /
> 日志 / panic / alloc）。`make test-qemu` 用最小 C 组件 `kcomp_c_smoke` 在 QEMU 上
> 端到端验证这条路径（CoreTest `c-frontend` 用例 + runner 的机器级 load/unload；RV64 + RV32）。
> （组件之间本就不允许 flat ELF symbol 互链，见 §2.1。）

### 2.3 SDK adapter 层：Alloc / Log / Panic 的归属

Core 管 Memory、不管 Heap：堆是 **runtime / deployment 策略**，不是 Core 资源（KernelNative 可共享 Core 内核堆，私有执行域可在自己的可写 `.data` / `.bss` 保留私有分配器），backing 以 region 粒度由 Core 提供（**不记 owner**）。adapter 层随 `.kcomp` 私有携带：

```text
GlobalAlloc   → component allocator adapter → kcore_memory_acquire / release → Core backing / mapping（无账本）
log crate     → component-local logger      → kcore_log_line
panic handler → component panic adapter     → kcore_log_line（打印诊断）+ kcore_panic_escape（协作式逃逸）
```

每次 instantiate 都从 artifact 独立放段 / 重定位，所以 `#[global_allocator]` 的 static 状态**天然 per-component**（各组件有自己的可写 image backing）；堆绑定是 runtime / deployment 策略，**不再经 per-instance runtime slot 或 `tp`**（`tp` 只是架构 / 任务执行状态，见 `docs/modules/arch.md`）。C 组件没有这些 Rust adapter，只 `#include "kcomp.h"` 直调 `kcore_*`，外加 SDK 的 freestanding `mem*` / `strlen` / `strchr`；边界刻意收紧，**不朝 libc 扩张**，也不是 shared runtime。契约见 `docs/architecture/memory-and-heap.md`。

## 3. ResourceDomain —— 一个"视图"，不是一个对象

**实现决策：ResourceDomain 第一版没有 struct。**

```text
ResourceDomain(ComponentId(7))
=
Core 里所有 owner == ComponentId(7) 的归属记录（device / irq / dma）
```

不写外置集合：

```rust
// ✗ 不要这样
struct ResourceDomain {
    devices: Vec<DeviceId>,
    irq_routes: Vec<IrqRoute>,
    dma_mappings: Vec<Mapping>,
}
```

而是资源自己的表记录 owner（数据库"视图"的直觉）：

```rust
struct DeviceTable {
    owner: [Option<ComponentId>; 256],
    quarantine: [bool; 256],
}

struct IrqRoute {
    owner: ComponentId,
    number: u32,
    handler: extern "C" fn(*mut ()),
    ctx: *mut (),
}

struct Allocation {            // DMA allocation（device-agnostic）
    base: usize,
    owner: ComponentId,
    lease: Option<MemoryLease>,
}

struct Mapping {               // DMA mapping（device-related）
    id: u64,
    owner: ComponentId,
    device_index: u8,
}
```

> **ResourceDomain 不记受管内存**：Core 不做内存记账（无 region owner 记录），只记录设备所有权 / IRQ route / DMA mapping，用于 revoke / teardown / quarantine；堆是 runtime / deployment 策略，不是 ResourceDomain 资源。契约见 `docs/architecture/memory-and-heap.md`。`ComponentId` 与 `DeviceId` 都是 identity（不是权限），所有权记录只存在于各资源表。

> **DMA 归属模型**：allocation 与 mapping 的归属 / 权限边界以 [驱动契约](driver-model.md#63-dma-模型allocation-与-mapping-分离) 为准；当前受信 Native mapping 锚在 device owner，caller 可与设备 owner 不同；提交仍复验双方生命周期。DeviceId 是身份，不能把记账推导为跨隔离域权限。

### 3.1 归属表可以非常普通

Core 不再有泛型 `Handle<T>` / `Slot<T>` / `ResourceTable<T>`。每张表只记原始归属与（必要时）一个永不误命中的 id：

```rust
struct DeviceTable { owner: [Option<ComponentId>; 256], quarantine: [bool; 256] }
struct IrqTable    { routes: [Option<IrqRoute>; 256] }        // 锚点 = device_index
struct DmaTable    { allocations: Vec<Allocation>, mappings: Vec<Mapping>, next_id: u64 }
```

control path（Core 校验归属，必须记录 trace）：

```rust
// 认领：已认领 / 已 quarantine → 拒绝；否则记 owner。
fn claim(caller: ComponentId, device_index: u8) -> Result<(), DeviceClaimError> { ... }

// 注册 IRQ route：只有 device owner 能注册。
fn register(caller: ComponentId, device_index: u8, handler, ctx) -> Result<(), IrqError> {
    if device_table.owner(device_index) != Some(caller) {
        return Err(IrqError::NotOwner);
    }
    ...
}

// 释放设备：non-owner 拒绝；仍有 live IRQ route / DMA mapping → -EBUSY。
fn release(caller: ComponentId, device_index: u8) -> Result<(), DeviceReleaseError> { ... }
```

**这些归属检查是"谁拥有 / 谁能拆"的记账，不是 per-access 鉴权**：`kcore_device_claim` 之后，driver 直接拿到裸 MMIO 指针，Core 不再参与每次寄存器读写（KernelNative 就是可信代码，见 `driver-model.md` §1.1）。

### 3.2 撤销归属

`revoke_owner` 按各资源表的 owner 处理可见记录，不要求外置 ResourceDomain 容器。
真实兜底汇合点是 `failure::revoke_authority_and_unbind`；顺序与 quarantine 规则只在
[驱动契约](driver-model.md#7-生命周期与-teardown-安全) 和 [生命周期](component-lifecycle.md)
维护。概念上的「撤销一组资源」不等于裸指针不可访问或物理资源已释放。

### 3.3 组件停止与失败：逻辑失效先于物理回收

| 路径 | 概念边界 | 权威细节 |
|---|---|---|
| Graceful stop | 先通过停止准入，组件 destroy 协作收尾，Core 兜底后提交 Stopped | [生命周期](component-lifecycle.md)、[驱动 teardown](driver-model.md#7-生命周期与-teardown-安全) |
| Failure | 先提交 Failed，不调用 destroy；撤销 Core 可见归属并 quarantine | 同上；不保证立即停止远端执行流 |
| Physical reclamation（未来） | 另证引用/执行者静止、页表/TLB 与设备 DMA 安全 | [内存与堆](memory-and-heap.md)、[服务执行 §6](service-execution.md#6-生命周期保证分别论证) |

设备自身 quiesce/reset 的顺序属于驱动；Core 已实现的兜底顺序属于生命周期机制。
KernelNative 的裸指针、挂死、恶意写内存、带锁失败不能由归属撤销强制隔离。
panic=abort 的 containment 没有 Rust Drop/unwinding 清理保证；失败不是 free(state)。
仍可能被 CPU 或设备访问的 backing 不得重新分配，CPU 静止也不证明 DMA 静止。

## 4. ExecutionDomain —— 部署属性与 Core 资源

执行域概念与能力以 [部署契约](deployment.md) / [驱动契约](driver-model.md) 为准。
KernelNative 共享 Core 地址空间；受限 IsolatedNative 使用私有 AS；SandboxedNative
组件创建仍显式 ENOTSUP。Native/Wasm 后端与 Inline/Queued 请求执行分别是正交维度。

### 4.1 实例记录与地址空间

`ComponentRecord` 直接拥有自己的 LoadedComponent、instance state、生命周期与部署属性，
AddressSpace 的存在/映射真相由 Core 内存模块维护。加载同一 artifact 两次就是两个
ComponentId 和独立可写 image；不再建 ComponentRuntime/ImageTable 复制 loaded 归属。
记录的精确字段见 [component 模块](../modules/core/component.md)，唯一身份契约见
[生命周期 §2](component-lifecycle.md#2-身份模型)。

### 4.2 loader / registry 与组合 Runtime

Core load 编排装载、登记、create、pending publication 提交与 Ready。
Resolved 是生命周期步骤，不证明已有通用 requires 解析器或运行时依赖图。
`init` / profile 负责选择、连接与初始化顺序；provider 的服务 Runtime 负责队列/Worker/
Session，均复用现有 ComponentId。library helper 与可独立部署的组件按实际需要区分，
不为了 ComponentManager 图再造第二套运行时 Registry。

### 4.3 KernelNative

create / destroy / Gate 通过 Core 管理的执行边界调用入口；Direct 数据面是已绑定的
C function table。后者不切 principal，也不建立单独的 panic containment，不能把函数
所属镜像当作资源请求的 owner。实例堆/状态仍保持自己的有效期，细节见生命周期 §7。

### 4.4 私有 AddressSpace 与执行域

Core 保存 AS 语义真相并提交映射，Arch 维护页表硬件投影。共享 Core 映射、import 面、
跨 AS trampoline 与条件性故障归因决定 Isolated 实际支持范围；S-mode 私有 AS 不是
不可信代码边界。普通 U-mode 用户 Task 已有，不等于 SandboxedNative 组件已可装载。
AS 退役、CPU/TLB 静止和物理 backing 释放分别论证，见 [内存与堆](memory-and-heap.md)。

### 4.8 按域装载

当前 KernelNative 使用 `loader.rs`；Isolated 使用 `isolated_load.rs` 按域放段/重定位并
由 `isolated_lifecycle.rs` 关联私有 AS 与执行入口。通用 ELF/白名单重定位复用既有逻辑。
不以一份 ET_REL 可解析就推断所有部署域的 import、设备访问和等待能力均可用。
源码入口与支持矩阵见 [component 模块](../modules/core/component.md) 和 deployment §10。

### 4.9 失败与退出的代码入口

真实入口是 [failure.rs](../../os/core/src/component/failure.rs) 的 `fail_component`
与 [exit.rs](../../os/core/src/component/exit.rs) 的 `stop_component`。
前者先 mark_failed 再兜底，后者在通过停止门禁后调用 destroy；二者都不承诺完整物理回收。
停止自己的 Task、解除客户端连接、排空业务请求与恢复策略分别由所属组件处理，
不能把一个示意 drop_runtime 当成实际可用的卸载原语。现状入口见
[component 模块](../modules/core/component.md)，规范见生命周期契约。

## 5. 生命周期

> **组件生命周期与组件 ABI 的冻结契约在 `docs/architecture/component-lifecycle.md`**（instance-aware 入口 `kcomp_instance_create` / `kcomp_instance_destroy`、`0 / -errno` 返回约定、`ComponentRecord` 直接拥有 `LoadedComponent` 的身份模型：一个 `ComponentId` = 一个完整运行组件）。本节只保留失败谱系与退出语义要点；冲突时以冻结契约为准。

所有组件共享统一生命周期（但**不共享**业务接口），单一真相 = `ComponentState::can_transition`：

```text
Declared → Resolved → Starting → Ready → (Stopping → Stopped) | Failed
```

`ComponentId` 永不复用；`Stopped` / `Failed` 记录留 tombstone，段内存不回收。
Core 可检查的入口拒绝过期身份；已缓存 Direct 表仍指向驻留代码，调用是否成功由
业务状态决定，Core 不保证撤回或逐次阻止。发布非空 Direct 表的 KernelNative 实例
保守拒绝 Stop；failure 仍会逻辑失效，不能据此释放 ctx。

### 5.1 失败谱系：Result 失败 vs panic

| 类别 | 表达 | 语义 |
|---|---|---|
| 普通方法失败 | `Result` / `-errno` | 向 caller 报错，不自动把 provider 标 Failed |
| 生命周期入口失败 | create / destroy 非零 | 按生命周期契约提交失败与兜底 |
| panic | `panic!`（`panic=abort`） | 仅已有 Core 管理边界可协作式收敛；不凭函数边界推导 recovery |

已有 Init / Task / Gate 边界的协作式 containment；Direct 与 IRQ 不自动获得同样的恢复边界。
不做 Rust unwinding，内存回收 deferred。panic recovery 不等于 fault isolation：
KernelNative 仍可能破坏 Core 内存 / UB / 带锁死亡；Isolated 的条件性故障归因也不等于
未来 U-mode 的不可信代码边界。细节见生命周期与部署契约。

### 5.2 退出语义

`Stopping` / `Stopped` 与 `kcomp_instance_destroy` 已接线：Core 侧唯一汇合点 = `component/exit.rs::stop_component`，生产调用方 = monitor `unload <name>` 与 `kcore_component_stop`。顺序：

```text
1. 拒绝门（提交之前；拒绝不改 Core 真相）：非 Ready；Native Direct publication；未退出任务；Core-managed inflight
2. begin_stop             Ready → Stopping：任务 run 门禁 + publish 拒绝
3. kcomp_instance_destroy Core-owned 隔离栈；ambient identity = 被停止实例
4. Core 兜底              撤销归属（device quarantine / DMA 停车）+ 失效 provider endpoint
5. finish_stop            Stopping → Stopped（记录保留）
```

失败路径刻意不调用 destroy；destroy 非零、panic 或栈分配失败均进入 Failed，不重试。
drain、Direct release、Sandbox 停止、实例回收、退出超时/看门狗仍未实现。
细节见冻结契约；不在概念页另维护一套门禁或终态规则。

## 6. Ownership Tree 与 Dependency DAG —— 两种关系，绝不混淆

整个系统**不是一棵树**，而是两种关系的叠加：

### Ownership Tree（生命周期归属）

描述"谁创建谁、谁负责谁的生命周期"：

下面是目标关系图，不表示 Core 已实现 parent/child 级联停止。当前 init 创建的组件
各自有独立生命周期；停止 init 不自动停止其消费者或所创建实例。

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

> 以下是目标分类与候选协议，不是现有 manifest 类型或热替换能力。新实例获得新
> ComponentId 与 endpoint；旧打开对象不自动转向新实例。职责见 [服务执行](service-execution.md)。

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
> 定义：重新实例化后的新 ComponentId 能进入 Ready；旧 Failed/Stopped 身份不复活。
> 是否重建业务状态与服务可用性由 recovery contract 决定。

### 候选替换流程（未实现，不做热迁移）

```text
quiesce → stop → unbind → reset → replace → bind → start
```

目标允许短暂中断；各环节要先证明静止、解绑与资源复用条件。物理帧分配器是
不可热卸载的 Core 内部机制，不属于此图。当前无 Direct release 或通用 drain，
失败设备仍 quarantine；不能宣称已有无重启驱动替换。live state migration 后置。

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
- 最底层 NVMe 是驱动组件：经 Core 认领设备（`kcore_device_claim`），向上提供 BlockDevice。

这一张图浓缩了全部模型：分层依赖（DAG）、驱动作为组件、Interface 语义化、以及未来把任意节点换成不同实现/执行域的可能性。

## 10. 参考（详见 references.md）

- Theseus：细粒度组件与状态归属、生命周期/替换建模；
- RedLeaf：语言级隔离与驱动恢复（ResourceDomain 回收）；
- Singularity：契约式通信（Interface 语义化）；
- Wasm Component Model：跨 ABI 接口描述（未来）；
