# 组件模型（component-model.md）

## 1. Component 是什么

Component 是 KaleidOS 的基本构建单元。它不只是"一个模块"，而是 **lifecycle 与 ownership 的同一单位（unit of lifecycle AND ownership）**：一个组件实例代表它的 code、execution、资源归属、interfaces、lifetime 与 failure state。因此它是一个完整的可管理单元：

- 消费（requires）和提供（provides）Interface；
- 拥有 ResourceDomain（Core 维护的资源集合）；
- 有生命周期（见 §5）；
- 可以依赖其他 Component；
- 可以包含子 Component（Composite，见 §7）；
- 可以被替换 / 重启 / 恢复。

一个实例因此可以拥有：tasks、stacks、claimed 设备、IRQ routes、DMA allocations/mappings、memory mappings、interfaces；失败时 Core 能按实例（per-instance）拆除它们（见 §3.3 与 §4.9）。

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

> Interface 是语义，传输是绑定策略。第一阶段用 Rust trait + direct call；
> 未来可换 IPC stub / Wasm host call。接口文档里写"契约"（方法、语义、错误），不写"怎么调用"。

### 2.1 绑定机制定案：Interface Registry（已落地骨架）

```text
Component → Core          = Core Export ABI（export.rs，ELF undefined symbol 白名单）
Component → Component     = Interface binding（interface.rs）——禁止 flat ELF symbol 互链
```

- 已加载组件的 exported ELF symbols **不组成全局符号表**：KaleidOS Component 是
  replaceable 的，直接 relocation 到 provider 函数地址会让替换非常困难。
- consumer 拿到的是**逻辑 binding**（`BindingId` + 当前 `api`/`ctx`/`generation`，
  其中 `api` 指向 provider 的 `#[repr(C)]` function table，`ctx` 是 provider opaque
  state），不是"永不变更的 provider ELF 符号地址"。provider 更换后 consumer 只需
  `refresh` 重新获取，**不需要 ELF reload**。
- **exact ABI fingerprint（`InterfaceAbi`，`#[repr(transparent)] u64`）取代
  version**：它没有版本兼容语义，只回答"provider 与 consumer 是否由完全相同的
  Service ABI contract 编译"。不一致必须拒绝 binding/replacement，绝不能把布局
  不同的 function table 交给 consumer。自动 ABI hash 生成器 / compatible range /
  ABI-changing coordinated update 属下一阶段（只留 seam）。
- Core 真相：`InterfaceRegistry` 记录 谁提供了什么接口（InterfaceId / abi /
  kind / provider / api / ctx / generation）；`publish` 在 `kcomp_instance_create()` 期间
  只记录 pending（**staged**），create 成功后 Core 原子提交；consumer `bind` /
  `refresh` 时 Core 再次校验 provider 存活（组件卸载/失败后 binding 立即不可用）。
- 阶段一 KernelNative 用 direct call / function table；传输升级（IPC / Wasm host
  call）不改 binding 数据模型。

### 2.2 `.kcomp` = 链接后的组件程序（目标：step 2-3）

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

- **third-party crate 是组件私有实现**：`smoltcp`、`virtio-drivers`、buddy allocator helper、协议 parser 一旦被组件使用，就成为 `.kcomp` 内部细节。Core 不认识 `smoltcp::socket::udp::...`，也不导出任何 Rust compiler/runtime 符号去满足组件；Core 只暴露固定 ABI（`kcore_heap_alloc/dealloc`、`kcore_log_line`、`kcore_device_*`、`kcore_irq_*`、`kcore_dma_*`、`kcore_task_*`、`kcore_panic_escape` ...）。
- **不建 shared Rust runtime**：不为所有 `.kcomp` 提供"shared core crate / shared alloc / shared fmt blob / shared runtime / component runtime symbol bag"去动态链接——那会把 rustc 版本、compiler 实现细节、monomorphization、内部 ABI 与 runtime state 变成系统 ABI。第一步接受每个组件**私有携带**它确实需要的少量 Rust support，再用 archive extraction / section GC / strip 压到最小；只有真实测量之后、且只针对极少数稳定能力，才允许提升进 Core ABI。
- **loader 不是 Rust dynamic linker**：它只做段放置 + 对白名单 `kcore_*` 的重定位，不理解 Rust 内部 ABI。

> 现状（step 2）：上述管线已落地，且已拆成「语言前端 + 语言无关 packer」两段：
> Rust 前端 `tools/build-kcomp.sh` 编出 `staticlib`（SDK / 依赖随镜像私有携带），
> C 前端 `tools/build-kcomp-c.sh` 编出 freestanding `.o`（clang，不链 libc），
> 两者都把输入交给 `tools/kcomp-link.sh` 做 partial link + section GC + strip，产出
> ET_REL `.kcomp`——因此 `.kcomp` 是**语言无关的组件二进制格式**，不是 Rust 格式。
> Makefile 与 `os/core/build.rs` 共用这些脚本，两条构建路径不再分叉；packer 在输出前
> 校验「ET_REL + `kcomp_instance_create` / `kcomp_instance_destroy` DEFINED + UNDEF 只有 `kcore_*` + 无 loader 不支持的重定位」。
> 组件通过共用 `kcomp-sdk`（§2.3）使用 ABI / 入口 / 日志 / panic adapter。
> （组件之间本就不允许 flat ELF symbol 互链，见 §2.1。）

### 2.3 SDK adapter 层：Alloc / Log / Panic 的归属

Core heap 是共享的（§3），但组件不裸调 Core 导出，中间有 SDK adapter 层：

```text
GlobalAlloc   → component allocator adapter → kcore_heap_alloc / kcore_heap_dealloc → Core heap
log crate     → component-local logger      → kcore_log_line                         → final sink
panic handler → component panic adapter     → kcore_log_line（打印诊断）+ kcore_panic_escape（协作式逃逸，见 §5） → —
```

> 这不是"每个组件自带一个堆"：每个组件有自己的 adapter（满足 Rust 类型/宏契约），但**底层资源仍由 Core 统一管理**。adapter 是 SDK / CRT 的一部分，随 `.kcomp` 私有携带。

> 现状（step 2）：`os/components/kcomp-sdk` 是这一层的落地——它是 `kcore_*` 导出白名单的
> 单一来源，提供 `kcomp_instance_create!` / `kcomp_instance_destroy!` 入口宏、`klog!` 日志（经 `kcore_log_line`）、组件私有
> `#[panic_handler]`（打印诊断后调 `kcore_panic_escape` 协作式逃逸），以及 feature `alloc`
> 下的 `#[global_allocator]`（接 Core 共享堆，无 per-component 堆）。SDK 是普通 library，
> 编译进每个 `.kcomp`，不是可加载组件、也不是 shared runtime。

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

> **KernelNative 的 Core 与组件共享一个 Core heap**：ResourceDomain **不**追踪 per-component 的堆分配或字节计费，也没有 per-component arena / 私有堆。它只记录设备所有权 / IRQ route / DMA mapping 和受管理的内存区域，用于 revoke / teardown / quarantine。
>
> `ComponentId` 是 identity（不是权限），`DeviceId` 也是 identity。所有权记录只存在于各资源表，两者已经明确分离。

> **DMA 归属模型（已决，刻意如此）**：`kcore_dma_map` 要求 caller 是**该设备的 owner**（Core 查 device 表，不接受组件自报设备号），并把 mapping 记在 device owner 名下；`kcore_dma_alloc` 本身是 device-agnostic 的，所以**不需要**也不接受"设备身份证明"。**不建模**“设备是不是 DMA master”：FDT 没有可靠来源（真实 QEMU virt DTB 只在 `/soc/pci@30000000` 标 `dma-coherent`），本阶段按**协作式信任**处理。**未决问题**：组件目前可以自己 claim 中断控制器（PLIC）等设备——“认领一台设备 = 拿到它的全部语义”这个边界还没有人回答；记录见 `docs/testing.md` §3。

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

### 3.2 回收：revoke_owner

```rust
fn revoke_component_resources(id: ComponentId) {
    device::revoke_owner(id);   // 失败路径：设备进 quarantine（保持到 reboot）
    irq::revoke_owner(id);
    dma::revoke_owner(id);      // backing 进 QUARANTINE（不 free）
}
```

每张表内部：

```rust
fn revoke_owner(owner: ComponentId) {
    for record in TABLE.iter_mut() {
        if record.owner == owner {
            revoke(record);
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
revoke_component_resources(id) —— Core 兜底，撤销剩余归属（device 进 quarantine / DMA backing 停车）
  ↓
ResourceDomain becomes empty
```

> 现状（§5.2 的第一版实现）：quiesce = `begin_stop`（`Ready → Stopping`，任务
> run 门禁 + publish 拒绝）、component-specific shutdown = 组件退出钩子
> `kcomp_instance_destroy`（monitor `unload` 触发）、stop = `finish_stop`；Core 兜底与失败
> 路径共用 `failure::revoke_authority_and_unbind`（剩余 device claim 进
> quarantine，DMA backing 停车）。有未退出任务的实例在第一步就被拒绝（drain variant 未实现）。

#### Forced containment（强制隔离）

组件 crashed / hung / 恶意行为时：

```text
Component Failed
  ↓
Core containment               —— 阻止它继续访问资源
  ↓
reset / isolate device（尽可能）
  ↓
force revoke ownership
  ↓
revoke_component_resources(id) —— 撤销归属记录（device / irq / dma）并做 quarantine
```

> 强制隔离撤销的是**归属记录**（device / IRQ route / DMA mapping），并对可能仍被设备访问的 DMA backing 做 quarantine。堆内存的清理走正常 Drop 路径；完整的内存回收属于 ExecutionDomain 的职责（见 §4）——phase 1 的 KernelNative 组件不承诺内存回收。

#### Teardown 是资源生命周期问题，不是 "free(stack) + done"

正确的拆除顺序以**资源生命周期**为中心：

```text
stop new work
  → quiesce / reset / detach device
  → mask IRQ
  → resolve outstanding interrupt claims
  → stop tasks
  → remove interfaces
  → unmap memory
  → wait / quarantine outstanding DMA
  → release resources
  → revoke ownership records
  → mark Failed / Destroyed
```

> 两条硬原则：
> 1. **任何仍可能被 CPU 或设备访问的物理内存，都不得重新分配**（否则就是 UAF / 数据破坏）。
> 2. **CPU 隔离 ≠ DMA 隔离**：即使 CPU 侧已停止访问、任务已停，设备 DMA 仍可能写入该内存——必须先 quiesce / wait / quarantine outstanding DMA，才能回收。

**意义**：restart、replace、fault recovery 全部建立在"Core 最终能收回 ResourceDomain"这一保证上。

## 4. ExecutionDomain —— 这里才真的有 enum

- **ResourceDomain** 回答"它拥有什么"；
- **ExecutionDomain** 回答"它在哪里运行"。

**两个 Domain 的形态故意不对称**：ResourceDomain 无 struct（§3，一个视图）；ExecutionDomain 是真正 owning 的 enum —— 它代表需要建立、切换、最终销毁的运行环境：

```rust
pub enum ExecutionDomain {
    KernelNative,
    IsolatedNative(AddressSpaceId),   // 可选实验（S + 私有 AS），非里程碑
    // future: SandboxedNative(AddressSpaceId)  —— U + 私有 AS，未来的硬件强制边界
    // future: Wasm(WasmInstance)
}
```

> **契约不能 ABI 锁定**：Interface 和 device claim / IRQ / DMA 机制必须与"传输方式"解耦，否则未来无法把组件挪进独立域。

### 4.1 不塞进 InstanceRecord

组件记录（**现状**：`InstanceRecord`，见 `docs/component-lifecycle.md`；旧的 `ComponentRecord` 已随 image/instance 拆分删除）本质是 Registry / monitor / inspection 用的 metadata；`AddressSpace` 是 heavyweight runtime 对象。两者不混：

```rust
pub struct InstanceRecord {
    pub id: ComponentId,          // 实例身份
    pub state: ComponentState,
    pub image: ComponentImageId,  // 代码 / 入口 / MemoryLease 归 image
    pub instance_state: *mut (),  // 组件私有的实例状态（create 返回）
}

// 未来若引入执行域，只在这里加一个轻量种类字段（**尚未实现**）：
//   pub execution_kind: ExecutionKind,   // KernelNative / IsolatedNative / …（未来字段，尚未实现；旧 ComponentRecord 已删除）
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

（概念代码；落地时按现有 Registry 状态机接轨。当前 load 链是
`Declared → resolve → Resolved → begin_start → Starting → call kcomp_instance_create →
{ failure → Failed | success → 提交 pending interfaces → finish_start → Ready }`：
`resolve()` 已落地（语义 = requires 全部绑定成功）；`Starting` 已接线为
`kcomp_instance_create()` 执行期（此期间 `kcore_interface_publish` 只记录 pending，不修改
active binding）。停止链已落地（§5.2：`Ready → Stopping → Stopped`，monitor
`unload` 驱动）；`Stopped` 记录保留、段内存不回收（phase 1）。）

> **Component Runtime ≠ Component**：Component Runtime 是负责 load / instantiate / 连接 registry / 管理 execution 与 lifecycle 的**基础设施**——可以是围绕 Core 的一组 library / manager（§4.1 的 `ComponentRuntime` struct 只是它持有的 per-component 运行时数据），但它本身**不是 Component**。同理，一个只为驱动组件提供共享机制的 "Driver Runtime"，首先也是 library / framework，不是 Component。
>
> 规则：不要因为有了组件模型就把一切都组件化。Component 对应真正具备 lifecycle / identity / ownership / execution / service-role 的实体（见 §1）。

### 4.3 KernelNative 具体是什么

`KernelNative` 只需要保存已加载镜像和执行种类，调用方式与现在一致：

```rust
let ret = containment::call_component_create(runtime.image.create, args, &mut out_state);
```

### 4.4 私有 AddressSpace 与执行域（未来 C10）

M0.5 的静态启动页表不是这里的 AddressSpace。真正的运行期地址空间在需要
**私有地址空间**时才引入（`IsolatedNative` / `SandboxedNative` 等执行域，或可执行回收），
由 Core 的 AddressSpaceManager 统一管理。

> **D2=A**：`KernelNative`（S + 共享内核 AS）是常态、长期模式，就是可信代码（无硬件访问强制，撤销协作式）；
> `IsolatedNative`（S + 私有 AS）是可选教学实验、**非里程碑**，只做条件性故障隔离；
> `SandboxedNative`（U + 私有 AS）才是未来的硬件强制边界。详见 `driver-model.md`。

```text
Core AddressSpaceTable
└── AddressSpaceSlot
  ├── owner / generation / lifecycle
  ├── semantic mappings
  └── opaque ArchSpace
    └── Sv39 root / PTE pages
```

`ExecutionDomain` 只保存 `AddressSpaceId`，不拥有可以绕过 Core 修改映射的页表
对象。Core 保存地址空间的语义真相；PTE 只是 backend 的硬件投影。

Core 的 map/unmap 是**内部提交点**（不导出给组件）：调用者只表达"要把哪个资源映射进自己的域"，目标地址空间由 execution domain 推导，最终映射由 Core 提交。

```text
map 提交（Core 内部）
  → validate region / overlap / permission
  → install backend mapping
  → commit mapping record and trace
```

物理内存的占用和区域归属由 Core 保存，不放进 PTE 的 RSW 字段；映射销毁也不
依赖 `Drop` 扫页表。私有、借用和共享关系由 Core 的 region/lifetime 记录表达，
页表 backend 只负责安装、撤销和激活硬件映射。backend 内部可以按 4 KiB 拆分，
但这不改变 Core 的 region 粒度。

映射和销毁必须是 Core 控制的显式事务：地址空间进入 `Dying` 后拒绝新操作，
停止引用它的任务，确认没有 CPU 正在使用，再由 backend 销毁页表，最后由 Core
按 ownership 回收资源并递增 generation。

具体的 backend contract 和 `ArchSpace` 所在 crate 仍需遵守当前依赖方向；在真正
实现 C10 前，不把 Core 绑定到 `Sv39`、`Pte`、`satp` 或某个具体 Arch backend。

### 4.8 Loader 自然分叉

现状 `load_component(blob)` 由 Core 侧 loader 编排：通用 ELF 对象解析在
`component/elf.rs`，段存储由 Core allocator 提供，架构/ABI 重定位由
`arch/riscv/elf.rs` 的 `RiscvRelocator` 实现（该实现可在 host 编译，测试直接驱动
生产代码）；loader 返回 `LoadedComponent { base, entry, text_size }`。
当前拿 PA 当 VA 拷贝。未来（按 ExecutionDomain 分叉）：

```rust
fn load_component(blob: &[u8], target: &mut dyn LoadTarget)
    -> Result<LoadedComponent, LoaderError>
```

- KernelNative target：alloc memory region → identity / kernel VA → copy；
- IsolatedNative target：向 Core 请求 memory region → Core 提交 range mapping → copy。

loader 不需要知道 satp / Sv39 / KernelNative / IsolatedNative，它只知道"给我一块能放 section 的 memory"——保持 arch / mechanism 分层。

### 4.9 失败与退出的实际代码流

```rust
pub fn fail_component(&mut self, id: ComponentId, reason: ComponentError) {
    self.registry.mark_failed(id).unwrap();
    self.stop_component_tasks(id);
    revoke_component_resources(id);
    self.drop_runtime(id);   // KernelNative: drop Rust state 结束；
                 // IsolatedNative: Core 显式销毁 AddressSpace
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
绝大部分资源由组件自己释放
  ↓
revoke_owner(id)   ← 只是保险："还有没释放的归属？有就 Core 扫掉（quarantine）。"
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
│   └── InstanceRecord
│       ├── id
│       ├── state
│       ├── image
│       └── instance_state
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
│       └── IsolatedNative(AddressSpaceId)
│
└── address_space.rs
  └── AddressSpaceManager / AddressSpaceSlot（未来 C10）


resource/    —— 概念分文件，不堆单文件：
  device.rs  —— DeviceTable{owner, quarantine}
  irq.rs     —— IrqTable{routes}（锚点 = device_index）
  dma.rs     —— DmaTable{allocations, mappings} + QUARANTINE
  每张表 record 带 owner: ComponentId
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
          KernelNative   AddressSpaceId
                        │
                Core-controlled backend
```

而 ResourceDomain 根本不在这棵树里：

```text
IRQ table  ─ owner=A ─┐
MMIO table ─ owner=A ─┼── ResourceDomain(A)
DMA table  ─ owner=A ─┘
```

### 4.11 落地顺序：现在只做两小步

1. **先不要写 ResourceDomain**。等 device/IRQ 真正开始做的时候，在每个资源归属 record 上加 `owner: ComponentId`，再留一个 `revoke_owner(ComponentId)` 就够了；
2. **完成 region allocation contract 后、真正需要隔离执行时**，再引入 `AddressSpaceManager`。
  它维护 `AddressSpaceSlot`、generation、语义 mapping ledger，并通过 Core 控制的
  backend 完成 map/unmap/activate/destroy；不使用 RSW ownership，也不依赖 `Drop`
  扫页表释放帧。

## 5. 生命周期

> **本节已被取代（superseded）：组件生命周期与组件 ABI 的冻结契约在
> `docs/component-lifecycle.md`。** 该文件定义 instance-aware 入口
> `kcomp_instance_create` / `kcomp_instance_destroy`、`0 / -errno` 返回约定与
> `ComponentImageId` + `InstanceRecord` 身份模型；本节保留原设计叙述，仅就地
> 更新事实性的 ABI 名称与签名。两者冲突时以冻结契约为准。

所有组件共享统一生命周期（但**不共享**业务接口）：

```text
Declared → Resolved → Starting → Ready → (Stopping → Stopped) | Failed
```

| 状态 | 含义 |
|---|---|
| Declared | 系统知道这个组件存在 |
| Resolved | 所有 requires 都已找到 provider（Registry `resolve()` 已落地；真实绑定在 Interface Registry，无 requires 时 vacuous 成立） |
| Starting | 正在初始化（执行 `kcomp_instance_create`） |
| Ready | 可以对外提供 Interface |
| Stopping | 正在停止：`kcomp_instance_destroy` 执行期（`Ready → Stopping` 由 `stop_component` 提交；任务被 run 门禁排除，publish 被拒，已有归属记录仍可 `release`） |
| Stopped | 已停止：`kcomp_instance_destroy` 已返回、剩余归属与接口已被 Core 兜底撤销（device 进 quarantine；记录保留；不回收段内存） |
| Failed | 运行过程中失败（可触发恢复流程；任何阶段都可能进入） |

> **`kcomp_instance_destroy`（Linux `module_exit` 类比）是组件 ABI 的对称退出入口**
> （`int32_t kcomp_instance_destroy(void *state)`，返回 `0 / -errno`）：Core loader 会
> **必需解析**该符号（`ComponentImage::destroy` / `LoadedComponent::destroy`；缺失即
> 加载失败）；monitor `unload <name>` 驱动的停止路径
> （`component/exit.rs::stop_component`）在实例 `Ready` 时调用它，并把实例推进
> `Stopping → Stopped`（第一版语义见 §5.2；drain variant / 实例退役仍开放）。
>
> **Failed 的恢复 = 逻辑重启**：标记 Failed、停止调度、在 Core 边界阻断过期访问、启动全新实例。phase 1 不承诺内存回收（KernelNative 无隔离）；完整回收留给未来 ExecutionDomain。
>
> **"阻断过期访问"的实际边界（重要，勿高估）**：它只对 **Core 经手的路径**成立
> —— `bind` / `refresh` / `claim` / IRQ 投递 / 调度都会查生命周期（provider 必须
> `Ready`，见 `component/interface.rs`；Core 每次调度决策都重新 `resolve_policy()`，
> 见 `os/core/src/sched.rs`）。它**不覆盖"consumer 手里已经拿到的裸函数表指针"**：
> 接口交付的是 `api: *const ()` / `ctx: *mut ()`，且 Core 明确"永不解引用"
> （`component/interface.rs` 顶部文档）—— 因此 consumer 若缓存了这张表，
> **组件 `Stopped`（甚至 `Failed`）之后调用仍会成功**，因为段内存未被释放。
> 这是"物理驻留 + KernelNative 无隔离"的直接后果，不是 bug。
>
> 收口方案（按代价）：①约定 consumer 每次调用前 `refresh(binding_id)` 重新取表
> （Core 内部已是此模式，但对外只是约定、非强制）；②Core 受控间接层
> （per-binding trampoline，唯一能在不换架构的前提下强制的做法）；③真回收 +
> per-domain 地址空间（`ExecutionDomain`，旧指针直接 fault）。三条的取舍与
> "热插拔到底做到哪一步"绑定，Linux 对照见 `references.md` 第 17 条。

> 意外退出 / abort 当前统一由 `Failed` 覆盖（组件 panic containment 路径）。未来
> 独立 abort/exit 通知的 hook 点见 `ComponentState::Failed` 的 `TODO(unexpected-exit)`。

### 5.1 失败谱系：Result 失败 vs panic（不虚构不存在的 recovery）

| 类别 | 表达 | 语义 |
|---|---|---|
| 普通失败 | `Result` / status code / `kcomp_instance_create() != 0` / `kcomp_instance_destroy() != 0`（返回 `0 / -errno`，见 §5.2） | 可恢复的**组件失败**，走正常 teardown / restart（§3.3） |
| 意外 panic | `panic!`（`panic=abort`） | 进程级 abort，不能凭空转成组件 recovery boundary |

> `panic=abort` 下，Core 栈上的普通 panic 不可能"魔法般"变成组件 recovery boundary。

**当前（phase 1）已有 init 边界 + task 边界的协作式 containment**：组件跑在 Core-owned 独立栈上；panic 时 Core 打印诊断，然后 stack-switch 回 Core 上下文，把该 task / instance 提交为 Failed（逻辑死亡，**不做 Rust unwinding**；内存回收仍 deferred）。

**panic recovery ≠ fault isolation**：KernelNative 组件仍可能破坏 Core 内存、制造 UB、持有裸指针、带锁死亡。真正的 memory-fault containment 是 IsolatedNative / U-mode 的职责（见 §4.4 与 `driver-model.md`）。

### 5.2 退出语义：第一版（small option）已实现

`Stopping` / `Stopped` 与 `kcomp_instance_destroy` 已接线：Core 侧唯一汇合点 =
`component/exit.rs::stop_component`，生产调用方 = monitor `unload <name>`
（组件侧参考实现 = `kcomp_smoke`：无资源可释放时也留一行"钩子已跑"的证据）。

**已实现顺序（small option，Oracle 评审后定稿）**：

```text
1. 拒绝门（任何提交之前；拒绝不改 Core 真相）
   a. 实例必须不拥有未退出的任务             → 否则 OwnsLiveTasks / EBUSY（不 join、不等待）
   b. 实例必须存在且处于 Ready（规则表）      → 否则 NotFound / ENOENT 或 NotReady / EINVAL
2. begin_stop                              Ready → Stopping：任务 run 门禁 + publish 拒绝
3. kcomp_instance_destroy（必需入口）        Core-owned 隔离栈；ambient identity = 被停止实例
4. Core 兜底                               撤销归属（device quarantine / DMA 停车）+ 解绑 provider（与失败路径同序列）
5. finish_stop                             Stopping → Stopped（记录保留）
```

- **为什么是这个顺序**：`may_run` 只允许 `Starting` / `Ready`，一旦提交
  `Stopping`，该实例的任务就不可能再被调度回来收尾——"先停后等任务"自相矛盾；
  `yield` 只提交 `Runnable`（不是 `Exited`），任务不会"自然退出"。
- **身份**：退出钩子跑在 Core-owned 临时栈上（与 `kcomp_instance_create` 对称，
  `containment::call_component_destroy`），其 Core 调用身份是**被停止的实例**
  （`EscapeKind::Exit`），不是发起 stop 的 monitor / 其他组件；
  `kcore_interface_publish` 在 exit 边界被拒（publish 是 init 期操作）。
- **归属**：组件应在钩子里 `release` 自己持有的东西（teardown 不受生命
  周期门禁限制）；钩子返回后 Core 仍兜底撤销一切**剩余**归属。剩余的
  device claim 会进失败 quarantine（撤销 ≠ 设备可安全复用），组件自己
  `release` 的不会——兜底与失败路径共用 `failure::revoke_authority_and_unbind`。
- **失败路径刻意不调用 exit**（Linux 类比：崩溃的模块不值得信任）：`Failed`
  只走 `fail_component`（mark + 兜底）。**未闭合**：组件侧的设备收尾
  （stop DMA / reset / mask IRQ）在失败路径上不会发生，Core 的 revoke +
  quarantine 是唯一兜底。
- **暂定默认（待人类定稿）**：`kcomp_instance_destroy` 返回非零（`-errno`）→ 镜像 `InitFailed`
  （`Failed` + 兜底）；钩子 panic → 镜像 `InitPanicked`（Exit 边界容纳 →
  `Failed`）。反方论证：退出失败可能不值得把实例标为逻辑死亡（它已经停了一
  半），也可以选择记录并继续 `Stopped`。
- **拒绝语义**：非 `Ready`（重复 stop / `Failed` / 未完成 init）→ `NotReady` /
  `EINVAL`；未知实例 → `NotFound` / `ENOENT`；有活任务 → `OwnsLiveTasks` /
  `EBUSY`（errno 映射见 `errno.rs::From<ComponentStopError>`）。

**仍开放（本版不做，需人类定稿）**：

1. **drain variant**（等任务清空再停）：需要 (a) `may_run` 增加"停止中仍允许
   收尾"的第三种语义，(b) 任务完成协议 / 超时 / 看门狗，(c) 与强制停止的关系。
   当前 small option 是"有活任务直接拒绝"。
2. **非零退出 / 退出 panic 的最终分类**：见上"暂定默认"。
3. **`UnexpectedExit` 是否作为独立终态**：当前意外退出统一 `Failed`；新增状态
   要改 `can_transition` 规则表与锚定它的 host 测试。
4. **信任域分叉**：本版退出钩子与 `kcomp_instance_create` 一样跑在 Core-owned 栈上
   （KernelNative、协作式、无隔离）；IsolatedNative / SandboxedNative 的停止
   （地址空间销毁、真正停止任务）是各自 ExecutionDomain 的职责。
5. **实例退役**：`Stopped` 记录保留（不回收段内存、`ComponentId` 不复用）；
   unload 记录 / 重新探测仍待定。
6. **退出期间的新资源认领门禁**：现有 export 门禁只拦 `Failed`；钩子在
   `Stopping` 期间仍可 `kcore_device_claim`（随后被兜底撤销）。硬拦需要把
   acquiring 门禁从"非 `Failed`"改成生命周期判定（会同时影响 `Stopped`）。
7. **退出钩子的阻塞 / 超时 / 看门狗**：钩子同步执行、无超时；挂死会挂住
   stop（KernelNative 协作式信任，与 `kcomp_instance_create` 同）。

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
- 最底层 NVMe 是驱动组件：经 Core 认领设备（`kcore_device_claim`），向上提供 BlockDevice。

这一张图浓缩了全部模型：分层依赖（DAG）、驱动作为组件、Interface 语义化、以及未来把任意节点换成不同实现/执行域的可能性。

## 10. 参考（详见 references.md）

- Theseus：细粒度组件与状态归属、生命周期/替换建模；
- RedLeaf：语言级隔离与驱动恢复（ResourceDomain 回收）；
- Singularity：契约式通信（Interface 语义化）；
- Wasm Component Model：跨 ABI 接口描述（未来）；
