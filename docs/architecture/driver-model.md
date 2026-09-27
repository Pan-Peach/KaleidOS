# 驱动与执行域模型（driver-model.md）

> 本文件是**驱动 / 设备认领 / 执行域 / IRQ 与 DMA 机制**的设计契约。它是设计文档，不是进度快照——哪些已落地见 `roadmap.md`。与 `architecture.md` / `component-model.md` 在驱动与执行域细节上冲突时，以本文件为准。

## 1. 定位与基线

驱动是 Component（或普通 Rust module），不是 Core 的一部分。它通过 Core 的 **mechanism** 访问设备：认领一台设备后拿到**本执行域下的访问窗口**，IRQ route 与 DMA mapping 由 Core 记账，再通过 Interface 向其他组件提供设备语义。

两条基线（取代旧的 Handle→Lease / authority 叙述）：

> Core 提供 mechanism，不伪造不存在的 security boundary。

> KernelNative 就是可信代码：Core 不替它做 per-access 鉴权，真正的访问强制只来自执行域。

展开：

- **Core 不是 capability 系统。** Core 只维护裸机程序无法自己知道的那部分真相：设备归谁、哪条中断线归哪个 owner、哪段 buffer 映射给哪台设备。这些记录服务于 **unload / 失败清理 / 防止重复认领 / teardown / quarantine**，**不是**用来"阻止一个受信组件"。
- **认领设备返回访问窗口**：KernelNative 下就是寄存器基址裸指针；Isolated（未来）下是 Core 把窗口映射进组件地址空间后返回的 VA。之后 driver 自己 `volatile` 读写，**steady state 不再进 Core**。
- **撤销是协作式的**：KernelNative 与 Core 同特权、共享内核地址空间，Core 无法追回已经交出的裸指针；"失效"以使用静默与翻译失效为准。Sandboxed / Isolated 域由地址空间映射 + 页表强制，撤销才能真正切断访问。

### 1.1 三个严格分离的概念

| 概念 | 是否做 | 说明 |
|---|---|---|
| **Security / access enforcement** | KernelNative **不做** | Isolated / Sandboxed（未来）的强制**只来自执行域**：私有地址空间 + 页表 + mapping + fault。**没有 per-access 软件 capability 校验**。 |
| **Ownership / lifecycle bookkeeping** | **做** | Core 记录哪台设备归哪个 Component、哪条 IRQ route 归哪个组件、哪段 DMA mapping 归哪个 device owner——用于 unload / 失败清理 / 重复认领防护 / teardown / quarantine。**不是**为了拦住可信的 KernelNative。 |
| **Mechanism** | **做** | 内存分配器、页表操作、IRQ 控制器路由、组件加载、执行上下文切换、DMA / IOMMU mapping backend、机器发现。 |

### 1.2 关于裸地址与裸指针

在 KernelNative：

- 组件**不能**通过"知道一个裸地址"获得任何东西——裸地址本身不表达所有权。但一旦 Core 在 `claim` 时把设备窗口交给调用方，**返回的就是裸 MMIO 指针**，driver 用自己需要的宽度（u8/u16/u32/u64）直接 `volatile` 读写。
- **旧的 `Handle → validate → Core MMIO read/write` 已删除**。对 KernelNative 而言它没有真实安全含义：Core 与 Core 校验的代码和 driver 同特权、同地址空间。
- 裸指针不是凭空构造的：它由 Core 在认领设备时解析已提交的 `MachineInfo.devices` 记录得出。

### 1.3 关于数据面

稳态寄存器 / 缓冲访问不需要 per-access 校验；补货、IRQ 投递、DMA map/unmap、claim/release 仍进 Core。

### 1.4 内存模型澄清（D1 已修订）

**Core 管 Memory，不管 Heap，也不做内存记账**：Core 对象堆只供 Core 内部使用；每个实例拥有独立 `HeapState`（共享的是分配器实现代码，不是堆）。Core 只以 **region / address-space 粒度**提供 backing / mapping 并推进生命周期，**不**记 owner（KernelNative 无账本；Isolated / Sandboxed 的归属由该实例的地址空间 / 页表承载），**不**做 per-instance 字节计费或配额。region release / instance failure 只保证**逻辑失效**、不承诺物理回收：KernelNative 已发布 backing 保留驻留，且因为没有归属记录，实例死亡也没有可回收之物。地址空间隔离解决的是访问强制，不是内存计费——**不得以"地址空间隔离"为名把 `HeapState` 分离说成安全隔离**。契约见 `docs/architecture/memory-and-heap.md`。

## 2. 三个执行域模型

| 模型 | 特权级 | 地址空间 | 目标 | 强制 |
|---|---|---|---|---|
| KernelNative | S | 共享内核 AS | 最高性能、常态 | 无硬件强制（可信代码） |
| IsolatedNative（可选实验，非里程碑） | S | 私有 AS | 生命周期恢复 / 内存回收 | 仅条件性故障隔离（协作与偶然 bug；对恶意无效） |
| SandboxedNative（未来） | U | 私有 AS | 对抗隔离 / 不可信代码 | 私有 AS + 页表 + 特权级 = 硬件强制 |

要点：

- **KernelNative 是正常、长期模式**：同域同特权级，调用即普通函数调用，零切换成本。Core 与驱动共享内核地址空间。
- **IsolatedNative 只是可选的 S-mode 教学实验**，用来观察生命周期恢复与内存回收，**不是里程碑**；S 与 Core 同特权级，天然不是恶意代码边界。
- **SandboxedNative 才是未来的强制边界**：U 模式 + 私有 AS + 页表，由硬件完成强制。
- **执行模型 / runtime（native machine code vs Wasm）是正交维度**：Wasm 只作为 Component 的**执行后端之一**（`AGENTS.md`），**不是第四个执行域**——`KernelNative` / `IsolatedNative` / `SandboxedNative` 都可以承载 Wasm runtime。见 `deployment.md` §3。

**部署形态本身就是安全策略，不是 Core 的统一强制。** 若不信任一个组件，就不要把它部署成 KernelNative。三个信任 / 执行域对应三种代价：

| 域 | 信任假设 | 代价 | 强制机制 |
|---|---|---|---|
| KernelNative | 受信 | 最快 | 无（可信代码） |
| IsolatedNative(S) | 半受信 | 原生性能 | CPU 故障隔离（私有 AS + 页表） |
| SandboxedNative(U) | 不可信 | syscall / 切换开销 | 硬件强制（私有 AS + 页表 + 特权级） |

Core 不应通过"把每个组件都塞进同一套重型安全机制"来解决信任问题——**部署形态的选择权（与责任）在系统组合者**。

> 执行域只决定"在哪里跑、谁来强制"；契约（Interface + mechanism）与执行域解耦，同一个组件图才能配置成宏内核 / 微内核 / 混合形态。

## 3. 设备认领（device claim）：identity → ownership → 访问窗口

```text
MachineInfo.devices（启动期静态发现）
        │  kcore_device_nth(compatible, ordinal)
        ▼
DeviceId（identity：可复制，不是 handle、不是 authority token、无 generation/slot）
        │  kcore_device_claim(device_id)
        ▼
Core：解析设备记录 → 独占检查 → 记 owner → 解析本执行域窗口
        │
        ├── KernelNative：裸寄存器基址（identity 映射）
        └── Isolated（未来）：映射进组件 AS 的 VA（未映射访问 → 页表 fault）
```

- `DeviceId` 是**身份**：从 `MachineInfo.devices` 的兼容匹配顺序解析而来，可自由复制、比较、透传；**不是** Handle、不可撤销、不携带权限、无 generation/slot。零可以是合法值。
- `kcore_device_claim(device_id)` 认领**那台确切设备**：Core 记 owner，返回本执行域下的可访问窗口（`(mmio, len)`）。独占锚在设备记录 index 上。
- **上层 driver 的寄存器访问逻辑不因执行域改变而重写**：KernelNative 返回裸指针，Isolated 返回 mapped VA，两者对 driver 同形（`DeviceMapping { mmio, mmio_len }`）。
- `kcore_device_release(device_id)` 主动释放；**拆机顺序**：仍有 live IRQ route / DMA mapping 时返回 `-EBUSY`——先静默设备、释放 IRQ/DMA，再释放 device。

### 3.1 设备发现是纯发现

`kcore_device_nth(compatible, len, ordinal, out_device_id)` 是**纯发现**：

- 只按已提交的 `MachineInfo` 列候选，**不分配、不触碰设备、不读 claim 状态、不授权**；
- 枚举**包含已认领设备**，顺序跨 claim/release 稳定；
- `ordinal >= 匹配数` → `-ENOENT`，这是枚举的**唯一终止信号**；
- `compatible` 对 Core 是**不透明匹配键**（Core 不理解协议语义）。

典型流程：driver 枚举候选 → 认领一个确切的 `DeviceId` → 读协议识别寄存器做**细匹配** → 不匹配则 release 并试下一个 ordinal。

### 3.2 可选的总线角色：driver_prober

`driver_prober` 是**可选的自动发现 / 自动加载策略 Component**，**不是 Core 基础设施**。它只做 compatible 级粗匹配，把候选 `DeviceId` 作为**选择数据**（不是资源）逐台下发给候选 driver。最小配置可以**跳过它**，直接调 `kcore_device_nth()`。

## 4. 模块边界

原则：**组件在 `os/components/`；驱动数量多、独立归纳在 `os/components/drivers/`；Core 缺的机制补进 `os/core/`。没有 `os/drivers/` 这种"外边"的第三处。**

```text
os/core/src/resource/
  mod.rs            ResourceKind（trace/记账标签）+ init
  context.rs        RequestContext（执行归属 + 生命周期所有权）
  device.rs         DeviceTable（owner + quarantine）；claim / release / resolve_mapping
  irq.rs            IrqTable（device-anchored routes）；register / enable / disable / release
  dma.rs            DmaTable（allocations + mappings）；QUARANTINE
os/core/src/memory/        MemoryLease（Core 内部 RAII）：buddy / region 分配
os/core/src/component/{isolated.rs, isolated_load.rs, isolated_lifecycle.rs}
                                     （受限 IsolatedNative 执行域：私有 AS + 跨 AS trampoline +
                                      按域放段 + Core 预置窗口；**没有** os/core/src/execution/ 目录）
os/core/src/component/{manager.rs, failure.rs} （manager.rs **目标**；failure.rs 现状）
os/components/                       政策 / 服务 / 测试 Component：
                                     scheduler_rr / core_test / driver_prober / …
os/components/drivers/               所有驱动 Component（统一归纳）：
                                     virtio_blk / …（uart **尚未实现**）
```

驱动组件直接用 Core 导出（`kcore_*`）；驱动需要的机制不足时，把机制补进 `os/core/`（并进 `export.rs` 白名单 + host 测试），而不是在 Core 外另立驱动层。第三方库的兼容适配（如 virtio 的 `Hal` 实现）作为**该驱动组件内部的模块**存在，不单独做成一层。

## 5. 数据结构（现状）

```rust
// 执行归属 + 生命周期所有权；不是 security principal（见 §5.1）
struct RequestContext {
    component: ComponentId,
    task: Option<TaskId>,
}

// —— Core 内部真相（不跨 ABI 暴露布局）——

struct DeviceTable {
    owner: [Option<ComponentId>; 256],
    quarantine: [bool; 256],          // 失败后保持到 reboot
}

enum IoSpace {
    Mmio { base: usize, size: usize },
    Pio  { base: usize, size: usize },   // x86 预留；本阶段不认领
}

struct DeviceDescriptor {
    space: IoSpace,
    irq: Option<u32>,                 // 单 IRQ 模型：一台设备一条线
    compatibles: [CompatStr; 4],
    compat_count: u8,
}

struct IrqRoute {
    owner: ComponentId,
    number: u32,
    handler: extern "C" fn(*mut ()),
    ctx: *mut (),
}

struct Allocation {
    base: usize,
    owner: ComponentId,
    lease: Option<MemoryLease>,       // None = 已 quarantine
}

struct Mapping {
    id: u64,                          // 单调递增，从不复用
    owner: ComponentId,               // = device owner（不是 ambient caller）
    device_index: u8,
}

enum DmaDirection { ToDevice, FromDevice, Bidirectional }  // ABI 0/1/2
```

说明：

- **`DeviceId` 是 identity**：可复制、可透传，不携带地址 / IRQ / 权限。它只在一个已提交 `MachineInfo` 的生命周期内有意义。
- **claim 返回 `DeviceMapping { mmio, mmio_len }`**：KernelNative 下是寄存器基址裸指针；driver 之后自己 `volatile` 读写。
- **`MemoryLease` 仍是 Core 内部 RAII ownership guard**（region / `PhysicalRange` + buddy allocator 的独占占用）。物理帧是 Core 内存分配器的实现细节——**组件永不跨 ABI manipulate "Frame #N"**。不要把 `MemoryLease` 描述成架构 capability，也不要把它与已删除的 `MmioView` / `DmaView` 混为一谈。
- **mapping id 是唯一保留的 "id" 对象**：单调递增 `u64`，从不复用，因为 DMA mapping 有真实的长生命周期（map → 设备使用一段 → unmap），stale id 自然查不到。
- **DMA allocation 的 owner 来自 ambient caller；mapping 的 owner 记在 device owner 名下**（provider 方法可能在 consumer 的任务上下文执行，映射仍应记到设备 owner，才能在 owner 失败 / 卸载时被回收）。
- **`RequestContext` 已降级**：仅表示执行归属 + 生命周期所有权，**不是** security principal，不做 capability 校验。

### 5.1 RequestContext 的定位（降级）

`RequestContext` / ambient component identity 仍然存在，但只用于两件事：

1. **执行归属**：这次 Core 调用是"替哪个组件做的"（解析最内层活动的 Core-managed 执行边界：create / task / IRQ scope）。
2. **生命周期所有权**：Core 把设备 owner / IRQ route owner / DMA allocation owner 记到哪个 `ComponentId`，以便 unload / failure 时回收。

它**不**证明调用者"有权"访问某个资源——KernelNative 是可信代码，没有可强制的能力边界可供它校验。IRQ 归属作用域（principal = 线 owner，task = None）同样是**记账**，不是认证。

## 6. API

### 6.1 Core 组件 ABI（精确集合）

```c
/* 发现（纯发现，不授权、不触碰设备） */
int32_t kcore_device_nth(const uint8_t *compatible, size_t len,
                         uint32_t ordinal, uint32_t *out_device_id);

/* 设备认领 / MMIO 窗口 */
int32_t kcore_device_claim(uint32_t device_id, uint8_t **out_mmio, size_t *out_len);
int32_t kcore_device_release(uint32_t device_id);

/* IRQ routes（锚点是 DeviceId；native callback only） */
int32_t kcore_irq_register(uint32_t device_id, void (*handler)(void *ctx), void *ctx);
int32_t kcore_irq_enable(uint32_t device_id);
int32_t kcore_irq_disable(uint32_t device_id);
int32_t kcore_irq_release(uint32_t device_id);

/* DMA（allocation 与 mapping 分离） */
int32_t kcore_dma_alloc(size_t size, uint8_t **out_ptr, size_t *out_len);
int32_t kcore_dma_free(uint8_t *ptr);
int32_t kcore_dma_map(uint32_t device_id, uint8_t *ptr, size_t len, int32_t direction,
                      uint64_t *out_device_addr, uint64_t *out_mapping);
int32_t kcore_dma_unmap(uint64_t mapping);
```

- 返回值统一 `0 / -Errno`（`os/core/src/errno.rs`）。有值的可失败入口一律 `status + out`。
- `direction ∈ {0=ToDevice, 1=FromDevice, 2=Bidirectional}`（与 SDK `DmaDirection` 一致）。
- **已删除**：`kcore_mmio_claim/read_u32/write_u32/release/lease`、`kcore_irq_claim`、`kcore_irq_register_polled`、`kcore_irq_poll`、`kcore_irq_ack`、`kcore_dma_lease`、`kcore_dma_release`。签名直接替换，本阶段不提供 ABI 兼容（所有内置组件一同重编）。

错误码（主要路径）：

- `kcore_device_nth`：`-ENOENT` ordinal 超出匹配数（唯一终止信号）；`-ENODEV` 机器信息未提交。
- `kcore_device_claim`：`-EPERM` 无法解析 caller 或 caller 已 `Failed`；`-ENODEV` 设备不存在；`-ENOTSUP` 设备是 PIO；`-EBUSY` 已认领或已 quarantine。
- `kcore_device_release`：`-EACCES` 非 owner；`-EBUSY` 仍有 live IRQ/DMA 子项；`-ENODEV` 不存在 / 未认领。
- `kcore_irq_*`：`-ENODEV` 设备不存在或无中断线；`-EACCES` 非 owner；`-EINVAL` enable 前尚未 register handler。
- `kcore_dma_alloc`：`-EINVAL` 尺寸非法；`-ENOMEM` 物理内存耗尽。
- `kcore_dma_map`：`-EINVAL` direction 非法或范围非法；`-ENODEV` 设备不存在；`-EACCES` 非 owner。
- `kcore_dma_unmap`：`-ENOENT` mapping 不存在。

### 6.2 IRQ 模型

- **锚点是已认领的 `DeviceId`**（`DeviceDescriptor` 自带 `irq: Option<u32>`），**不是** `IrqHandle`。单 IRQ 设备下再套一层"MMIO → IRQ 派生"没有真实用途，已删除。
- Core 只维护 **IRQ line / owner / callback / context**：
  - `kcore_irq_register` 记录一条 route（handler + opaque ctx），只有设备 owner 能注册；
  - `kcore_irq_enable` / `kcore_irq_disable` 配置中断控制器（arch 层），表锁只覆盖验证，PLIC 寄存器在**锁外**写；
  - `kcore_irq_release` 撤销 route 并关断控制器线——此后不再投递给已死 owner。
- **投递**：`trap → Core route → native callback`。Core 在锁内只取一份 `(owner, handler, ctx)` 拷贝，回调在**锁外**执行。回调运行在 Core 建立的 **IRQ 归属作用域**内（principal = 该线的 owner、`task = None`），被中断的边界在回调返回后恢复。作用域同步、不可 yield；作用域内调度类 Core 调用返回 `-EINVAL`；回调内 panic **致命**（没有 Core 拥有的可恢复上下文）。
- **已删除（defer）**：`Polled` / 计数 / 掩蔽 / `ack` 的 event-delivery 模型。那属于真实 isolated / U-mode 执行模型出现后才需要的机制；当前 KernelNative 只走最简单的 native callback。
- **暂不引入多 MSI-X vector / shared line / 跨 owner delegation**：真实需求出现再加 `irq_index` 或动态 IRQ 身份。

### 6.3 DMA 模型：allocation 与 mapping 分离

```text
allocation（device-agnostic）            mapping（device-related）
  kcore_dma_alloc(size)                    kcore_dma_map(device_id, ptr, len, dir)
    → CPU-visible buffer (ptr, len)          → device-visible address + mapping id
  kcore_dma_free(ptr)                      kcore_dma_unmap(mapping)
```

- **分配是 device-agnostic 的**：`kcore_dma_alloc(size)` 只要求后端给一块**物理连续**内存，不知道 VirtIO / NVMe / 具体 `DeviceId`。未来后端可以换 DMA pool / buddy / low-memory / coherent / bounce-buffer pool，上传接口不变。
- **映射是 device-related 的**：`kcore_dma_map(device_id, ptr, len, direction)` 返回 `(device_addr, mapping_id)`。只有设备 owner 能把 buffer 映射给该设备。
  - **No-IOMMU：`device_addr` 就是 buffer 地址（identity）**；
  - 未来 IOMMU 在**同一 seam** 内把 PA → IOVA；受限设备地址经 bounce buffer。**上层 driver 不变**。
- **`mapping_id` 单调递增 `u64`，从不复用**：这是唯一保留的 "id" 对象，理由是 DMA mapping 有真实的长生命周期（map → 设备使用 → unmap）。
- `kcore_dma_free` / `kcore_dma_unmap` 做拆除。

### 6.4 未来 syscall 线格式（SandboxedNative 方向）

**三个域不得因为"做同一件事"就共用同一个底层 ABI。** 语义（allocate / map / irq / log / interface-call）可以复用，**transport 必须分开**：

| 域 | transport | 形态 |
|---|---|---|
| KernelNative | 直接调用 | narrow `extern "C"` C ABI（见 §6.1） |
| IsolatedNative | 受控边界 | 私有 AS 边界 + 受控入口 |
| SandboxedNative(U) | syscall | 自有稳定 wire ABI |

**不要**把 KernelNative 的 Rust/C 接口原样搬到 U-mode 复用——下面 `a7/a0..a5` 线格式就是 U-mode 专属的稳定 ABI。

```text
a7 = op
a0..a5 = args
a0 = status
a1 / a2 = 结果低 / 高 32 位
```

用户指针逐页按托管映射校验，**不依赖 `SUM`**（不允许"内核直接访问用户指针"的捷径）。

## 7. 生命周期与 teardown 安全

```text
quiesce（静默设备）→ unmap → safe free
        │  无法确认设备已静默
        └──────────────► quarantine（不归还、不复用）
```

- **组件失败 ≠ 设备已静默**。设备可能仍在 DMA 往某块内存写；立即 free / 复用会让设备写进已被重新分配的区域。无 IOMMU 时 Core **无法确认设备已静默**，因此 backing lease 一律 move 进 Core 私有 `QUARANTINE`（**不归还 buddy heap**）。
- **这是 correctness，不是 security**：quarantine 保护的是"设备仍可能在写的内存不得被重新分配"这条正确性不变式，不是"阻止恶意组件"。
- **优雅拆除的顺序**：先静默设备，再 `kcore_dma_unmap` 撤销映射，确认无未结清后 `kcore_dma_free`；顺序不成立就 quarantine。没有"确认设备静默"的手段时，本版**不 free**。
- **设备 quarantine**：失败路径把该组件占用的每台设备标记进 Core 的 device quarantine（保持到 reboot）；优雅 `release` 不标记，设备可复用。
- **拆机顺序**：仍有 live IRQ route / DMA mapping 时 `kcore_device_release` 返回 `-EBUSY`。

两个硬限制（必须在设计里明说，不能假装不存在）：

1. **全局恒等 MMIO 映射会让"移除派生映射"失效**：只要存在恒等别名，unmap 派生映射并不能真正切断访问。要么去掉别名，要么明文接受**仅协作式撤销**。
2. **unmap MMIO 不会停 DMA / 撤销已发出的设备写**：设备可能在映射撤销后继续写。因此 DMA 回收要求**设备静默**（reset / 确认无未结清）。

## 8. 调用链

### KernelNative MMIO

```text
device_nth → device_claim → 直接拿到寄存器基址 → 驱动 volatile 直访（稳态不进 Core）
```

### IRQ

```text
claim 设备 → irq_register(device_id, handler, ctx) → irq_enable(device_id)
  → trap → Core route(number) → 锁外 native callback（IRQ 归属作用域）
  → irq_disable / irq_release
```

### DMA

```text
dma_alloc → (ptr, len)          # device-agnostic
dma_map(device_id, ptr, len) → (device_addr, mapping_id)
  → 驱动用 ptr 访问 / 设备用 device_addr
dma_unmap(mapping_id) → dma_free(ptr)      # 失败时 quarantine
```

### 沙箱（未来）

```text
每域 root（不含恒等 RAM / 全局 MMIO；RX / RW-NX + guard）
  → U 入口
  → ecall
  → Core 映射 / 授权
  → fault 杀域
```

### 跨组件 Interface

同域直接 vtable；跨域走 domain-aware thunk。

## 9. 驱动兼容策略

- **virtio 优先 KernelNative**。
- **不 fork `rcore-os/virtio-drivers`**：在 **virtio 驱动组件内部**实现其 `Hal`（组件内 `compat/virtio.rs` 模块），用 claim 得到的 MMIO 基址 + `kcore_dma_*` 接入。
- **VirtIO HAL 的当前接线**：

  ```text
  dma_alloc   = kcore_dma_alloc(内存) + kcore_dma_map(device_id, buffer)
  share       = kcore_dma_map(device_id, buffer)     # 把现有 buffer 映射给设备
  unshare     = kcore_dma_unmap(mapping)
  dma_dealloc = kcore_dma_unmap + kcore_dma_free
  ```

  driver 存储 **claimed `DeviceId` + MMIO 基址**（原子变量），驱动内部用 `DMA_MAP: device_addr → mapping id` 做极薄 bookkeeping；**旧的全局 `MMIO_HANDLE` DMA 归属已删除**。No-IOMMU 下 device address 就是 buffer 地址。
- **IsolatedNative(S) 不做**；未来要真正隔离时把驱动搬进 SandboxedNative(U)——主要换 transport / loader，不重写驱动逻辑。
- **第三方 crate 兼容性实测结论**：`virtio-drivers 0.13.0` 的**类型 / 解码面**能直接编进 no_std rv64/rv32 的 `.kcomp`（不需要 alloc，ET_REL 只剩 `kcore_*` UND）；但 `MmioTransport` 需要一个裸的 `NonNull<VirtIOHeader>` 基址、`Hal` 需要 DMA 与地址翻译——这两件正是 **claim 返回的裸 MMIO 指针 / `kcore_dma_alloc + kcore_dma_map`** 要补的 Core 能力。
- **无共享 Rust runtime**：每个 `.kcomp` **私有携带自己的 Rust 支撑**。不建立共享 Rust runtime 符号袋，不提供共享 alloc / 共享 fmt 供组件链接；组件可链接的外部符号只有 Core 白名单 `kcore_*`（见 §4：第三方库的兼容适配作为该驱动组件内部模块）。

### 9.1 驱动组件如何挂载（就是组件间依赖）

设备发现与驱动挂载不是一套独立框架，而是已有的组件依赖模型：

```text
boot:  FDT → MachineInfo → DeviceDescriptor（设备发现）

runtime:
  ① 先挂上一个 driver component（总线 / probe 角色，如 virtio-mmio bus）做初始化
  ② 它枚举 / 探测设备（compatible、device_id）
  ③ 通过 Component Endpoint Registry（lookup / bind）解析并启动匹配的
     设备驱动 component（如 virtio_blk）
  ④ 设备驱动 component 向 Core `kcore_device_claim` / `kcore_irq_register` /
     `kcore_dma_alloc`（device_id 就是认领锚点）
  ⑤ 驱动 provides Device Interface（如 BlockDevice），供上层 Service 消费
```

这与 `component-model.md` 的 Dependency DAG（NVMe → BlockDevice → FS…）是同一回事，不需要额外的 driver framework。

## 10. 分阶段计划与测试

| 阶段 | 内容 | 测试 |
|---|---|---|
| 1 | device claim → 裸 MMIO 指针（KernelNative identity）；`RequestContext` 归属 | claim 越界 / PIO / 重复认领被拒；release 后设备可复用 |
| 2 | IRQ route（device-anchored）+ native callback + 编排式 `fail_component` | 失败前排队的 IRQ 不会在重启后执行失败实例的回调；无 handler 就 enable → 拒绝 |
| 3 | DMA alloc/map 分离 + 设备静默 + quarantine | free 后 backing 不归还 buddy / 不被重新分配；越界 map 被拒 |
| 4 | 第一个受限域（优先 U 模式；S 私有可选） | 组件内存 fault 返回错误给调用者、保住另一组件、可重启且不泄漏 owned region |
| 5 | 需要时加 timer 控制 / H·PMP·IOMMU | 非协作域无法阻止 Core 夺回；IOMMU 用越界 DMA fault |

## 11. 已知限制

- **CPU 隔离 ≠ DMA 隔离**：即使组件拥有私有地址空间，若设备具 bus-master DMA 且平台无 IOMMU，**页表拦不住设备**。因此不得以"我们有独立地址空间"为由宣称"完整安全隔离"——那只是 CPU 侧隔离。设备侧隔离要么靠 IOMMU（阶段 5），要么明文接受不做。
- **无 IOMMU**：DMA 只能做偶然故障隔离，无法阻止恶意 / 失控设备越界。
- **KernelNative 撤销是协作式**：Core 能撤销 ownership 记录，但不能回收正在死循环的受信代码，也追不回已经交出的裸指针。
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

- `resolve_mapping` 的 Isolated 分支：私有地址空间下把设备窗口映射进组件 AS 的确切接口（返回 VA、页表权限、失败路径）——当前只有 KernelNative identity；
- 设备窗口的粒度 / 对齐约束是否需要在 `DeviceMapping` 之外显式建模；
- 多 MSI-X vector / shared IRQ line 的身份表示（真实需求出现再加 `irq_index` 或动态 IRQ 身份）；
- `RequestContext.task` 在非任务上下文的 IRQ / 设备路径上恒为 `None` 的精确契约；
- **组件间设备 ownership 转移 —— 仍然刻意 deferred**：让一个总线组件把已认领的设备交给另一个 driver，需要 Core 支持跨组件所有权转移 + 组件寻址（组件彼此拿不到 `ComponentId`）。当前用"prober 交选择数据（`DeviceId`）、driver 在自己的执行上下文里 claim 同一设备"解决设备选择；真正的 ownership 转移属"完整 capability 系统"，语义未定（move / copy、能否降权、能否再传递、撤销如何传播）；
- **动态驱动更换 / 更新 —— 刻意 deferred（独立里程碑）**：re-probe 只是"detach → 退休旧组件 → attach 新组件"事务的后半段。优雅停**已落地**（入口 `kcomp_instance_destroy`，见 `docs/architecture/component-lifecycle.md`）；旧组件退役后其 loaded backing 仍 **pinned-until-reboot**。真正缺的是**更换事务本身**：re-probe 编排、driver 受控排空 / 静默 / 释放子资源，且失败路径把设备 quarantine 到 reboot。hot-plug、多组件加载、竞争驱动优先级、Core match table / 运行期 driver registration 同样 deferred；
- 阶段 5 中 timer 控制、H·PMP、IOMMU 的具体接口。

### 12.1 已决：设备选择（原 Q1）

原模型里设备认领只做"第一台 compatible 匹配且未被认领"，QEMU 上 `virtio,mmio` 有 8 台同 compatible 设备，组件无法**精确选择**。现定案：

- `kcore_device_nth(compatible, len, ordinal, out_device_id)` 纯发现（不分配、不触碰设备、包含已认领设备、order 稳定；`ordinal >= 匹配数` → `-ENOENT` 是唯一终止信号），产出 `DeviceId`（identity，非 handle）；
- `kcore_device_claim(device_id, ...)` 认领**那台确切设备**，独占锚在 device index；
- `kcore_irq_register(device_id, ...)` 与 `kcore_dma_map(device_id, ...)` 都以 `DeviceId` 为锚点（IRQ 通过 `DeviceDescriptor.irq` 解析线号），不存在"MMIO 给 A、IRQ 给 B"的跨设备错配；
- `kcore_device_release` 在仍有 live IRQ route / DMA mapping 时 `-EBUSY`；`kcore_irq_release` 撤销 route 并关断控制器线；
- 失败 containment：`fail_component` 把失败组件占用的每台设备标进 Core 的 quarantine（phase 1 保持到 reboot；优雅 `release` 不标记）。

读 virtio-mmio `DeviceID` 本身需要 claim 后的 MMIO 访问：探测是"**独占 claim → 识别 → release → 下一个 ordinal**"，不做无副作用的只读探测 claim（generic MMIO 读可能清状态 / 弹 FIFO，Core 不学协议语义）。组件级 **prober**：`os/components/driver_prober` 是**协议无关**的总线角色——只有一张 opaque 候选目录（`compatible → 候选组件`）+ prober-owned assignment cursor，**不 claim MMIO、不读任何寄存器、不解释 compatible**。流程（**无环**，step 4）：prober 粗匹配（`kcore_device_nth`）→ 逐台把 `(device_id, 结果端口名)` 编码成**扁平 create config**（`abi/probe.toml` 的 `DriverCreateConfig`）→ `kcore_component_create` 候选组件 → driver 在**自己的 create 上下文**读 config、claim + 读协议识别寄存器做**细匹配**，把 `Match` / `NoMatch` staged publish 到 `probe.result` endpoint（不匹配 = 正常，release 后 create 成功返回）→ prober 在 create 返回 0 后**拉取**该 endpoint、用自己的本地调用更新 cursor。driver **绝不回调 prober**：旧的 assignment 回调 Service（prober 暴露回调、driver 在 create 中调用）形成 `Task(prober) → Driver create → Service(prober)` 同步重入环，已被 create config + 结果 pull 取代。

## 13. 已决

- **D1（已修订）**：Core 管 Memory、不管 Heap，也不做内存记账；每个实例拥有独立 `HeapState`（共享分配器实现代码，不共享堆）；Core 只以 region 粒度提供 backing / mapping，**不**记 owner（KernelNative 无账本；Isolated / Sandboxed 归属由该实例的 AS / 页表承载），**不**做 per-instance 字节计费；region release / instance failure 只保证逻辑失效、backing 保留驻留（不承诺物理回收，无归属记录时实例死亡亦无可回收之物）；**不得以"地址空间隔离"为名把 `HeapState` 分离当成安全隔离**。契约见 `docs/architecture/memory-and-heap.md`。
- **D2 = A**：KernelNative 是正常、长期模式；IsolatedNative（S + 私有 AS）是可选的**教学实验**、**不是里程碑**；SandboxedNative（U + 私有 AS）是未来的**强制边界**。执行模型 / runtime（native vs Wasm）是**正交维度**，Wasm 只是 Component 的执行后端之一，不是第四个执行域。
- **设备访问模型**：`DeviceId`（identity）+ `kcore_device_claim` 返回本执行域窗口；KernelNative 拿裸寄存器基址，driver 自己 `volatile` 读写；不做 per-access 鉴权。旧的 `Handle → validate → Core MMIO read/write`、typed `MmioLease` / `DmaLease` 已删除。
- **IRQ 模型**：锚点是已认领的 `DeviceId`；只支持 native callback；polled / count / mask / ack 已删除并推迟到真实 isolated / U-mode 执行模型。
- **DMA 模型**：allocation（device-agnostic）与 mapping（device-related）分离；mapping id 单调递增 `u64` 从不复用；No-IOMMU identity，IOMMU / bounce buffer 在同一 seam。
- **DMA teardown = correctness**：组件失败 ≠ 设备静默；backing lease 不归还 buddy，move 进 Core 私有 `QUARANTINE`；没有"确认设备静默"的手段时不 free。
- **`MemoryLease` 定位**：Core 内部 RAII ownership guard；物理帧是 Core 实现细节，组件永不跨 ABI manipulate "Frame #N"。
- **`RequestContext` 定位**：执行归属 + 生命周期所有权，**不是** security principal。

## 14. 相关文档

- `architecture.md`：分层、Core 边界、ResourceDomain / ExecutionDomain 总览；
- `component-model.md`：组件生命周期、ResourceDomain 视图、AddressSpaceManager；
- `roadmap.md`：执行域/隔离的里程碑位置（C10 进行中；IsolatedNative 是可选实验、非承诺里程碑）；
- `references.md`：seL4 typed capability、Theseus 状态归属等借鉴来源（注意 KaleidOS **不**实现 capability 系统，只借用"资源真相在 Core"的思想）。
