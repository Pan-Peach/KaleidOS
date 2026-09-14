# 驱动与执行域模型（driver-model.md）

> 本文件是**驱动 / Handle→Lease / 执行域 / 撤销不变式**的设计契约。它是设计文档，不是进度快照——哪些已落地见 `roadmap.md`。与 `architecture.md` / `component-model.md` 在驱动与执行域细节上冲突时，以本文件为准。

## 1. 定位与基线

驱动是 Component（或普通 Rust module），不是 Core 的一部分：它通过 Core 授予的类型化 authority 访问设备，通过 Interface 向其他组件提供设备语义。本文件固定三件事——驱动如何获得 authority、执行域如何决定强制程度、撤销何时算完成。

两条基线（原文保留）：

> Core owns authoritative resource records. The execution backend determines which accesses are enforced; trusted native code must uphold the rest.

> Handles authorize resource derivation; mappings and DMA leases enable scoped fast paths. Revocation completes only after users and devices are quiescent and relevant translations are invalidated.

展开：

- Core 存权威资源记录：存在性 / 状态 / 所有权 / 生命周期 / **映射来源（provenance）**。
- **execution backend 决定哪些访问被硬件强制**。KernelNative 是受信代码，未强制部分靠自觉（逻辑 authority）；只有 SandboxedNative 等才由硬件强制。
- Handle 授权的是资源**派生（derivation）**：从已持有的 authority 推导出新的 scoped 访问能力，而不是直接给出裸地址。
- mapping / lease 提供 **scoped fast path**：稳态访问不经过 Core。
- 撤销完成 ≠ generation 变了；必须在"用户与设备静默、相关翻译失效"之后才算完成。

### 1.1 关于裸地址、裸指针与 Handle 的真实含义

组件不能通过知道一个裸地址来获得 authority；裸指针只允许存在于 Core 派生并持有 provenance 的 typed Lease 实现内部。

**Handle 属于控制面（control plane），不是快路面（fast path）**：它承载资源身份 / 所有权 / authority / generation / 生命周期 / 撤销 / 记账 / 清理。快路面是**经 Core 校验一次的原生指针 / 映射**。

在 KernelNative，**Handle 不是内存安全屏障**：若受信组件已持有裸 MMIO 指针，撤销 handle 并不能魔法般阻止那个已泄漏的指针继续生效。Handle 表达的是"Core 授予你对这个资源有 authority"，**不是**"CPU 在物理上禁止你绕过我"。硬件层面的阻止只属于 SandboxedNative（见 §2、§3）。

### 1.2 关于数据面

稳态寄存器/缓冲访问不需要 per-access 校验；补货、IRQ 投递、映射变更仍可能进 Core。

### 1.3 内存模型澄清（D1=A）

Core 与组件共享**一个 Core heap**，不做 per-component 内存记账；只有**显式拥有的运行期区域（owned runtime region）可回收**，共享堆上的分配需要**显式清理**。**不得以"地址空间隔离"为名引入 per-component 私有堆**——地址空间隔离解决的是访问强制，不是内存计费。

## 2. 三个执行域模型

| 模型 | 特权级 | 地址空间 | 目标 | 强制 |
|---|---|---|---|---|
| KernelNative | S | 共享内核 AS | 最高性能、常态 | 无硬件强制（逻辑 authority） |
| IsolatedNative（可选实验，非里程碑） | S | 私有 AS | 生命周期恢复 / 内存回收 | 仅条件性故障隔离（协作与偶然 bug；对恶意无效） |
| SandboxedNative（未来） | U | 私有 AS | 对抗隔离 / 不可信代码 | Handle + MMU + 特权级 = 硬件强制 |
| Wasm（更远） | — | — | 可移植沙箱 | 运行时强制 |

要点：

- **KernelNative 是正常、长期模式**：同域同特权级，调用即普通函数调用，零切换成本。Core 与驱动共享内核地址空间。
- **IsolatedNative 只是可选的 S-mode 教学实验**，用来观察生命周期恢复与内存回收，**不是里程碑**；S 与 Core 同特权级，天然不是恶意代码边界。
- **SandboxedNative 才是未来的强制边界**：U 模式 + 私有 AS + Handle 校验，由硬件完成强制。
- **Wasm 更远**，只作为 Component 的执行后端之一。

**部署形态本身就是安全策略，不是 Core 的统一强制。** 若不信任一个组件，就不要把它部署成 KernelNative。三个信任 / 执行域对应三种代价：

| 域 | 信任假设 | 代价 | 隔离机制 |
|---|---|---|---|
| KernelNative | 受信 | 最快 | 无（逻辑 authority） |
| IsolatedNative(S) | 半受信 | 原生性能 | CPU 故障隔离 |
| SandboxedNative(U) | 不可信 | syscall / 切换开销 | 硬件强制（Handle + MMU + 特权级） |

Core 不应通过"把每个组件都塞进同一套重型安全机制"来解决信任问题——**部署形态的选择权（与责任）在系统组合者**。

> 执行域只决定"在哪里跑、谁来强制"；契约（Interface + Handle）与执行域解耦，同一个组件图才能配置成宏内核 / 微内核 / 混合形态。

## 3. Handle → Lease（authority vs execution capability）

**Handle = authority；Lease = execution capability。** 更准确地说：**Handle 在控制面**（身份 / 所有权 / authority / generation / 生命周期 / 撤销 / 记账 / 清理），**Lease 是快路面**（经 Core 校验一次的原生指针 / 映射）。Handle 不承诺"CPU 物理禁止绕过"（见 §1.1）。

```text
MmioHandle（authority）
        │  Core 校验：owner / generation / 范围 / 生命周期
        ▼
MmioLease { 私有 ptr, len, source: handle@gen }
        │
        ▼
驱动直接 volatile 访问（稳态不 per-access 进 Core）
```

- Handle 表示"Core 承认你对这个资源有权威"；可被派生、传递、撤销。
- Lease 把 authority 变成可执行访问能力的 scoped 对象：它内部可以持有裸指针，但该裸指针不可由组件凭空构造，其 provenance 绑定 `Handle + generation`；执行能力经 `as_ptr()` / `len()` 暴露，`source()` 回传派生自的 handle。
- 撤销 Handle 后，已派生 Lease 失效；**KernelNative 下撤销是协作式的**——Core 不能追回已经交出的裸指针（同地址空间、同特权级），"失效"以使用静默与翻译失效为准（见 §7），不是 generation 一改就自动完成；Sandboxed / Isolated 域由地址空间映射 + 页表强制，撤销可真正切断访问。

## 4. 模块边界

原则：**组件在 `os/components/`；驱动数量多、独立归纳在 `os/components/drivers/`；Core 缺的机制补进 `os/core/`。没有 `os/drivers/` 这种"外边"的第三处。**

```text
os/core/src/handle/
  context.rs        RequestContext
  mmio.rs           claim / derive_lease / release
  dma.rs            DmaTable / DmaRegion / DeviceAddr
  irq.rs            IrqDelivery（含 Polled）
  lease.rs          MmioLease / DmaLease
os/core/src/memory/address_space.rs   AddressSpaceManager 接线；Mapping 加 MappingSource + lifetime
os/core/src/execution/{mod.rs, sandboxed.rs}
os/core/src/component/{manager.rs, failure.rs}
os/components/                       政策 / 服务 / 测试 Component：
                                     scheduler_rr / core_test / logger / …
os/components/drivers/               所有驱动 Component（统一归纳）：
                                     uart / virtio_blk / …
```

驱动组件直接用 Core 导出（`kcore_*`）；驱动需要的机制不足时，把机制补进 `os/core/`（并进 `export.rs` 白名单 + host 测试），而不是在 Core 外另立驱动层。第三方库的兼容适配（如 virtio 的 `Hal` 实现）作为**该驱动组件内部的模块**存在，不单独做成一层。

## 5. 数据结构

```rust
struct RequestContext {
    component: ComponentId,
    task: TaskId,
    domain: ExecutionDomain,
}

struct MmioRecord {
    owner: ComponentId,
    range: PhysRange,
    device_index: u32,
    generation: u32,
}
type MmioHandle = Handle<MmioRecord>;

struct MmioLease {
    ptr: *mut u8,
    len: usize,
    source: HandleRef,       // handle@generation
    owner: ComponentId,
    target: /* 待定，见 §12 */,
}

enum MappingSource {
    Anonymous,
    Mmio { slot: u32, generation: u32 },
    Dma  { slot: u32, generation: u32 },
}

struct Mapping {
    virtual_range: VirtualRange,
    physical_range: PhysicalRange,
    permission: /* 抽象权限 */,
    source: MappingSource,
    lifetime: /* 待定，见 §12 */,
}

enum DmaDirection {
    ToDevice,
    FromDevice,
    Bidirectional,
}

struct DmaRegion {
    device_index: u8,        // 从 caller 已持有的 MmioHandle 推导（owner 记在 slot 上）
    size: usize,             // 实际 backing 容量（MemoryLease region size，≥ 请求）
    direction: DmaDirection,
    device_addr: usize,      // 设备可见地址；v1 identity == 物理基址（无 IOMMU）
    lease: Option<MemoryLease>,  // backing 真相；release/revoke 时 move 进 QUARANTINE
}
type DmaHandle = Handle<DmaRegion>;

struct DmaLease {
    ptr: *mut u8,            // backing 指针（Core 派生，provenance 绑定 source）
    len: usize,
    device_addr: usize,      // 设备可见地址（v1 identity == ptr，无 IOMMU）
    source: DmaHandle,       // slot + generation
}

enum IrqDelivery {
    Polled,                              // 未来沙箱域只允许这一种
    Callback { handler: ..., ctx: ... }, // 仅受信 KernelNative
}

struct IrqEventState {
    owner: ComponentId,
    line: IrqId,
    count: u64,
}

struct ComponentRuntime {
    id: ComponentId,
    image: LoadedComponent,
    execution: ExecutionDomain,
    owned_regions: Vec<MemoryLease>,
}
```

说明：

- `device_index` **不能由组件自由填写**，必须从它已经持有的设备 / MMIO authority 推导，避免伪造设备身份（`kcore_dma_alloc` 只收 `MmioHandle`，不收设备号）。
- **v1 无 IOMMU：`device_addr` 就是物理基址（identity）**。设备可见地址通过 `DmaLease` 暴露给受信 KernelNative（`kcore_dma_lease` out 参数），组件不能自行指定物理/设备地址。
- **DMA 撤销 = 协作式 + quarantine**：`release` / `revoke_owner` 把 backing `MemoryLease` move 进 Core 私有 `QUARANTINE`（**不 free**），再 revoke slot（handle → Stale）。设备可能仍在 DMA，权威回收需要设备静默（reset / 确认无未结清），本版 deferred（见 §7）。
- `Mapping.source` 记录映射来自哪个 authority slot + generation，撤销时据此识别**别名**。
- `IrqDelivery::Callback` 只允许受信 KernelNative；未来沙箱域只能用 `Polled`。
- `owned_regions` 让"组件显式拥有的运行期区域"可被枚举回收，但不改变 D1=A（共享堆仍无 per-component 计费）。

## 6. API

### 6.1 Core 内部（统一收 `&RequestContext`）

```text
mmio::grant / derive_lease / release
dma::alloc / device_addr / release
address_space::map_source / unmap_source
irq::event_count
memory::quarantine
failure::fail_component
```

所有入口收 `&RequestContext`，由它给出 `component / task / domain`；authority 校验以组件 ID + generation 为准，不从裸地址推断。

### 6.2 KernelNative ABI v4 增量

```text
kcore_device_nth(compatible, len, ordinal, out_device_id)   # 纯发现：列候选，不授权
kcore_mmio_claim(device_id, out_handle)                  # 认领确切设备（root authority）
kcore_mmio_read(handle, offset, width, out)
kcore_mmio_write(handle, offset, width, value)
kcore_mmio_release(handle)              # 有 live IRQ/DMA 子 authority 时 -EBUSY
kcore_mmio_lease(handle, out_ptr, out_len)
kcore_dma_alloc(mmio_handle, size, direction, out_handle)  # 设备身份从 MmioHandle 推导
kcore_dma_lease(handle, out_ptr, out_len, out_device_addr)
kcore_dma_release(handle)
kcore_irq_claim(mmio_handle, out_handle)   # 从 MmioHandle 派生同台设备的 IRQ
kcore_irq_register / enable / register_polled / poll / ack
kcore_irq_release(handle)                     # 真正关断投递 + 控制器线
```

**设备身份 / 认领链（identity → root → derived）**：`DeviceId`（`u32`，`MachineInfo.devices`
中的记录身份）**不是 Handle、不可撤销、无权限**；`kcore_device_nth` 是纯枚举（不分配、
不触碰设备、包含已认领设备，order 跨 claim/release 稳定；`ordinal >= 匹配数` → `-ENOENT`
是唯一终止信号）。`kcore_mmio_claim` 用 `DeviceId` 认领**确切设备**（不再"第一台
compatible 匹配"），独占锚在 `device_index` 上；`kcore_irq_claim` / `kcore_dma_alloc`
都从调用方已持有的 `MmioHandle` 派生 `device_index`——不存在"MMIO 给 A、IRQ 给 B"的
跨设备错配。**签名直接替换**：`kcore_mmio_claim` / `kcore_irq_claim` 沿用原名、签名变更，
本阶段不提供 ABI 兼容（所有内置组件一同重编；loader 只按名字解析、不校验签名，故不保证
陈旧 `.kcomp` 可加载）。错误码：无 MMIO（PIO）设备 → `-ENOTSUP`；设备无 IRQ → `-ENODEV`；
已认领 → `-EBUSY`；无效/stale/wrong-owner 的 MMIO handle → `-EBADF`/`-ESTALE`/`-EACCES`。

- `width ∈ {1, 2, 4, 8}`；`direction ∈ {0=ToDevice, 1=FromDevice, 2=Bidirectional}`；错误沿用 v3 约定（`0` 成功 / `-Errno`）。
- **`kcore_mmio_lease` 进入直接 ABI**：Core 校验 handle 后一次性派生 `(ptr, len)` + provenance（`source` handle），受信 KernelNative 驱动据此直接访问。
- **`kcore_dma_alloc` / `kcore_dma_lease` 进入直接 ABI**：`dma_alloc` 用调用方**已持有的 `MmioHandle`** 推导设备身份、由 Core 分配物理连续 backing（**不接受自报设备号**）；`dma_lease` 派生 `(ptr, len, device_addr)`——**设备可见地址由 Core 在 DMA 授权时给出，经 typed lease 暴露给 KernelNative**（v1 identity：== 物理基址，无 IOMMU），组件不能自行指定物理/设备地址；`map` 仍不进直接 ABI，映射由 Core 提交。
- **DMA 撤销**：`kcore_dma_release` 只 revoke authority，backing 内存进 Core 私有 quarantine（**不 free**）；权威回收需设备静默，本版 deferred（见 §7 / §11）。

### 6.3 未来 syscall 线格式（SandboxedNative / Wasm 方向）

**三个域不得因为"做同一件事"就共用同一个底层 ABI。** 语义（allocate / map / irq / log / interface-call）可以复用，**transport 必须分开**：

| 域 | transport | 形态 |
|---|---|---|
| KernelNative | 直接调用 | narrow `extern "C"` C ABI（见 §6.2） |
| IsolatedNative | 受控边界 | 私有 AS 边界 + 受控入口 |
| SandboxedNative(U) | syscall | 自有稳定 wire ABI |

**不要**把 KernelNative 的 Rust/C 接口原样搬到 U-mode 复用——上方 `a7/a0..a5` 线格式就是 U-mode 专属的稳定 ABI。

```text
a7 = op
a0..a5 = args
a0 = status
a1 / a2 = 结果低 / 高 32 位
```

用户指针逐页按托管映射校验，**不依赖 `SUM`**（不允许"内核直接访问用户指针"的捷径）。

## 7. 生命周期与撤销不变式

```text
Grant → Derive lease → Use → Quiesce → Unmap（含别名）→ TLB fence → Free | Quarantine → Revoke
```

- **撤销完成 = 未结清使用已结束 + 相关翻译已失效**（不是 generation 变了）。
- 地址空间状态：`Ready → Dying → Destroyed`；进入 `Dying` 后拒绝新操作、停止引用它的任务、确认没有 CPU 正在使用，再由 backend 销毁页表。
- 组件进入 `Dying` 后拒绝新操作。
- `Quarantine` 用于"不确定是否仍被设备/硬件引用、因而不能立即复用或释放"的区域。

两个硬限制（必须在设计里明说，不能假装不存在）：

1. **全局恒等 MMIO 映射会让"移除派生映射"失效**：只要存在恒等别名，unmap 派生映射并不能真正切断访问。要么去掉别名，要么明文接受**仅协作式撤销**。
2. **unmap MMIO 不会停 DMA / 撤销已发出的设备写**：设备可能在映射撤销后继续写。因此 DMA 撤销要求**设备静默**（reset / 确认无未结清）。

## 8. 调用链

### KernelNative MMIO

```text
claim → derive_lease → 驱动 volatile 直访（稳态不进 Core）
```

### IRQ（Polled 协议）

```text
PLIC → trap → Core/Arch claim(line)（authority 在 Core）
     → delivery == Polled：events ++；
       首事件在锁外 InterruptImpl::disable(line)（软件掩蔽，防电平触发风暴）
     → Core/Arch complete(line)
     → 驱动任务轮询 kcore_irq_poll（读计数，判 delta）
     → 服务设备
     → kcore_irq_ack：清 masked → Core 锁外 InterruptImpl::enable(line) 重新放行
```

- **不在被打断的上下文里跑 polled 组件的回调**：trap 顶半部只做计数 + 掩蔽 + `complete`，全部 arch 调用在 IRQ 表锁外；组件逻辑延后到驱动任务上下文。
- **软件掩蔽闭环**：首事件掩蔽该线，电平触发源在协作调度下不会反复打断；驱动 `ack` 后 Core 才重新使能。组件拿不到中断号，掩蔽/放行 authority 全在 Core。
- `IrqDelivery::Callback`（受信 KernelNative）保持旧行为：锁外 inline 调用 handler，再 `complete`。两态互斥（`delivery` 是 `Option<IrqDelivery>`）。
- **暂不引入 block/wake**。

### DMA

```text
alloc → device_addr → KernelNative 用 lease 指针
```

失败时静默或 `quarantine`。

### 沙箱（未来）

```text
每域 root（不含恒等 RAM / 全局 MMIO；RX / RW-NX + guard）
  → U 入口
  → ecall
  → Core 授权
  → fault 杀域
```

### 跨组件 Interface

同域直接 vtable；跨域走 domain-aware thunk。

## 9. 驱动兼容策略

- **virtio 优先 KernelNative**。
- **不 fork `rcore-os/virtio-drivers`**：在 **virtio 驱动组件内部**实现其 `Hal`（组件内 `compat/virtio.rs` 模块），用 `MmioLease` + `DmaLease` + IRQ 轮询事件接入。
- **IsolatedNative(S) 不做**；未来要真正隔离时把驱动搬进 SandboxedNative(U)——主要换 transport / loader，不重写驱动逻辑。
- **第三方 crate 兼容性实测结论**：`virtio-drivers 0.13.0` 的**类型 / 解码面**能直接编进 no_std rv64/rv32 的 `.kcomp`（不需要 alloc，ET_REL 只剩 `kcore_*` UND）；但 `MmioTransport` 需要一个裸的 `NonNull<VirtIOHeader>` 基址、`Hal` 需要 DMA 与地址翻译——这两件正是 `MmioLease` / `DmaLease` 要补的 Core 能力。**真正的 virtio 驱动组件要等 step 1–3。**
- **无共享 Rust runtime**：每个 `.kcomp` **私有携带自己的 Rust 支撑**。不建立共享 Rust runtime 符号袋，不提供共享 alloc / 共享 fmt 供组件链接；组件可链接的外部符号只有 Core 白名单 `kcore_*`（见 §4：第三方库的兼容适配作为该驱动组件内部模块）。

### 9.1 驱动组件如何挂载（就是组件间依赖）

设备发现与驱动挂载不是一套独立框架，而是已有的组件依赖模型：

```text
boot:  FDT → MachineInfo → DeviceRecord（设备发现）

runtime:
  ① 先挂上一个 driver component（总线 / probe 角色，如 virtio-mmio bus）做初始化
  ② 它枚举 / 探测设备（compatible、device_id）
  ③ 通过 Component Interface Registry（requires / bind）解析并启动匹配的
     设备驱动 component（如 virtio_blk）
  ④ 设备驱动 component 向 Core claim MmioHandle / IrqHandle / DmaHandle
  ⑤ 驱动 provides Device Interface（如 BlockDevice），供上层 Service 消费
```

这与 `component-model.md` 的 Dependency DAG（NVMe → BlockDevice → FS…）是同一回事，不需要额外的 driver framework。

## 10. 分阶段计划与测试

| 阶段 | 内容 | 测试 |
|---|---|---|
| 1 | RequestContext + typed `MmioLease`（含 provenance） | revoke 后通过旧映射访问必须 fault，而不是打到被复用资源（覆盖恒等别名） |
| 2 | MMIO write/release + IRQ 计数轮询事件 + 编排式 `fail_component` | 失败前排队的 IRQ 不会在重启后执行失败实例的回调 |
| 3 | `DmaLease` + 设备静默 + quarantine | 超时 / reset 失败后，未结清的 DMA 存储不可被再次分配 |
| 4 | 第一个受限域（优先 U 模式；S 私有可选） | 组件内存 fault 返回错误给调用者、保住另一组件、可重启且不泄漏 owned region |
| 5 | 需要时加 timer 控制 / H·PMP·IOMMU | 非协作域无法阻止 Core 夺回；IOMMU 用越界 DMA fault |

## 11. 已知限制

- **CPU 隔离 ≠ DMA 隔离**：即使组件拥有私有地址空间，若设备具 bus-master DMA 且平台无 IOMMU，**页表拦不住设备**。因此不得以"我们有独立地址空间"为由宣称"完整安全隔离"——那只是 CPU 侧隔离。设备侧隔离要么靠 IOMMU（阶段 5），要么明文接受不做。
- **无 IOMMU**：DMA 只能做偶然故障隔离，无法阻止恶意 / 失控设备越界。
- **KernelNative 撤销是协作式**：Core 能撤销 authority，但不能回收正在死循环的受信代码。
- **协作调度下，非协作 / 死循环域无法被夺回控制**；完整夺回依赖 timer 抢占 / 特权级强制（阶段 5）。

**能力诚实（capability honesty）**：KaleidOS 必须按轴显式描述平台能力，**不得用"可适配"掩盖硬件限制**。每个平台至少声明：

| 轴 | 取值 |
|---|---|
| CPU 内存隔离 | yes / no |
| DMA 隔离 | yes / no |
| 特权隔离 | yes / no |

执行域承诺的强制程度必须**落在平台真实能力之内**（例如无 IOMMU 时，SandboxedNative 的承诺只覆盖 CPU 侧，不含设备侧）。

## 12. 待定 / 开放问题

以下为**尚未定案**的内容，不在此处臆造：

- `MmioLease` 是否进入直接 ABI —— **已决**：进入，`kcore_mmio_lease(handle, out_ptr, out_len)` 返回 Core 校验过一次的 `(ptr, len)`，provenance 由 `source` handle 持有；`device_addr`（DMA 设备可见地址）仍不进入，见 §6.2；
- `MmioLease.target` 的确切语义（目标设备？目标组件？还是仅作调试标注）；
- `Mapping.lifetime` 的表示（引用计数 / 显式 grant ID / 其它）；
- `DmaRegion.direction` 的枚举形状 —— **已决**：`DmaDirection { ToDevice, FromDevice, Bidirectional }`（ABI 编码 0/1/2）。同时定案：设备可见地址 v1 identity（== 物理基址，无 IOMMU），经 `DmaLease` 暴露给受信 KernelNative；DMA 撤销为协作式 + quarantine（设备静默前不 free，见 §5 / §7）；
- `kcore_irq_poll` 的精确签名与返回值形状（单事件 / 计数 / 批量）——**已决**：采用「计数」，配套 `kcore_irq_register_polled` / `kcore_irq_ack`，见 §13；
- `RequestContext.task` 是否可为空（非任务上下文的 IRQ/设备路径）；
- **设备选择（Q1）—— 已决**：原先 `kcore_mmio_claim` 只做"第一台 compatible 匹配且未被认领"，QEMU 上 `virtio,mmio` 有 8 台同 compatible 设备，组件无法**精确选择**；`kcore_irq_claim` 也独立按 compatible 匹配，可能与 driver 认领的 MMIO **不是同一台设备**。定案采用"**纯枚举 + 精确认领 + 从 root 派生**"：
  - `kcore_device_nth(compatible, len, ordinal, out_device_id)` 纯发现（不分配、不触碰设备、包含已认领设备、order 稳定；`ordinal >= 匹配数` → `-ENOENT` 是唯一终止信号），产出 `DeviceId`（identity，非 Handle）；
  - `kcore_mmio_claim(device_id, out_handle)` 认领**那台确切设备**，独占锚在 `device_index`；
  - `kcore_irq_claim(mmio_handle, out_handle)` 与 `kcore_dma_alloc(mmio_handle, ...)` 都从调用方已持有的 MMIO root 派生 `device_index` / irq，**不再**独立匹配 compatible；
  - `kcore_mmio_release` 在仍有 live IRQ/DMA 子 authority 时 `-EBUSY`（root 生命周期）；`kcore_irq_release` 真正关断投递与控制器线；
  - 失败 containment：`fail_component` 把失败组件占用的每个 `device_index` 标进 Core 的 quarantine（phase 1 保持到 reboot；优雅 `release` 不标记）。
  读 virtio-mmio `DeviceID` 本身仍需 MMIO 访问（claim + lease）：探测是"**独占 claim → 识别 → 释放 → 下一个 ordinal**"，不做无副作用的只读探测 claim（generic MMIO 读可能清状态 / 弹 FIFO，Core 不学协议语义）。组件级 **prober（Q1 的用法层）—— 已落地**：`os/components/driver_prober` 是**协议无关**的总线角色——只有一张 opaque 候选目录（`compatible → 候选组件`，如 `virtio,mmio → virtio_blk`）+ prober-owned assignment cursor，**不 claim MMIO、不读任何寄存器、不解释 compatible**。流程：prober 粗匹配（`kcore_device_nth`）→ 请求 `kcore_component_load` 候选组件 → 经通用 **assignment Service**（`driver.prober`：`next_assignment`/`report_attempt`，只传 `DeviceId`/attempt 数据、不传 authority）逐台下发候选 → **driver 在自己的执行上下文** claim + 读协议识别寄存器做**细匹配**（不匹配 = 正常，release 后继续；分派耗尽 = init 成功、无设备）。即"粗匹配 → 加载候选代码 → 细 probe → attach"；在 driver 代码执行前不可能完成最终硬件匹配（协议知识 + authority 必须在 driver 上下文），这是职责划分而非含糊。
- **组件间 authority 转移（handle transfer，Q2）—— 仍然刻意 deferred**：让 prober 把自己已认领的 `MmioHandle` 直接交给 driver，需要 Core 支持**跨组件所有权转移**；更根本的缺口是**组件寻址**——组件彼此拿不到 `ComponentId`（Core 不允许自报身份），Interface Registry 的 binding 也不是可寻址 id。属"完整 capability 系统"，语义未定（move / copy、能否降权、能否再传递、撤销如何传播）。**本轮只解决设备选择（Q1）**：prober 释放后把选中的 `DeviceId` 作为**选择数据**（不是 authority）经通用 assignment Service（`driver.prober`）交给 driver，driver 在**自己的执行上下文**里 claim 同一 `DeviceId`——这不是 handle transfer（`RequestContext::ambient()` 从执行任务/init 上下文推导身份，prober 调 driver vtable 不会转移 authority）。当前 §9.1 的"设备驱动自己向 Core claim"继续有效；待出现真实 broker 场景（如 FS server 把块设备能力转交给另一个 server）再定 Q2。
- **动态驱动更换 / 更新 —— 刻意 deferred（独立里程碑）**：re-probe 只是"detach → 退休旧实例 → attach 新实例"事务的**后半段**，不是更换本身。真正缺的是**优雅 teardown**：现有组件只有 init / 任务退出 / 失败，没有"优雅停"，且失败路径把设备 quarantine 到 reboot——不能拿"手动清 quarantine"当更新手段。未来需要通用组件生命周期入口 `kcomp_exit()`（Linux `module_exit` 类比，已定义为 ABI seam，尚未驱动）+ `Stopping`/`Stopped` 状态、旧实例退休（释放名字槽、分配新 `ComponentId`）、driver 受控排空/静默/释放子 authority，之后才谈 re-probe。hot-plug、多实例加载、竞争驱动优先级、Core match table / 运行期 driver registration 同样 deferred。
- 阶段 5 中 timer 控制、H·PMP、IOMMU 的具体接口。

## 13. 已决

- **D1 = A**：保留"单一共享 Core heap、不做 per-component 记账"；只有显式拥有的运行期区域可回收；共享堆分配需要显式清理；**不引入 per-component 私有堆**。
- **D2 = A**：KernelNative 是正常、长期模式；IsolatedNative（S + 私有 AS）是可选的**教学实验**、**不是里程碑**；SandboxedNative（U + 私有 AS）是未来的**强制边界**；Wasm 更远。
- **`kcore_irq_poll` 签名/语义**：`i32 kcore_irq_poll(u64 handle, *mut u64 out_count)`（`0 / -Errno`，计数走 out 参数）读取该线**累计**事件数（不清零，驱动按 delta 判断）。配套 `kcore_irq_register_polled(u64 handle)` 把线切成轮询投递、`kcore_irq_ack(u64 handle)` 清 `masked` 并由 Core 在锁外重新放行该线（`InterruptImpl::enable`）。单事件 / 批量两种形状不采用。

## 14. 相关文档

- `architecture.md`：分层、Core 边界、ResourceDomain / ExecutionDomain 总览；
- `component-model.md`：组件生命周期、ResourceDomain 视图、AddressSpaceManager；
- `roadmap.md`：执行域/隔离的里程碑位置（C10 非当前里程碑）；
- `references.md`：seL4 typed authority、Theseus 状态归属等借鉴来源。
