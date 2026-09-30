# memory 与 heap：Core Memory ↔ Runtime Heap

> 本文件是**内存资源（Core）**与**堆（运行时）**的权威契约。
> 一句话：**Core 管 Memory，不管 Heap；唯一例外是 KernelNative 共享 Core 堆的窄部署后端。**
> 与 `AGENTS.md` 的不可违背原则一致；访问窗口与 `driver-model.md` 的 device claim **同形**——两者都返回「本执行域访问窗口」。

## 1. 分层

| 层 | 负责 | 不负责 |
|---|---|---|
| **Core** | Memory：backing / mapping（按需给 backing；Isolated 时把 region 映射进该实例的 AS）；**KernelNative heap 后端**：`kcore_heap_alloc/dealloc` 薄接既有 `KernelAllocator` | **owner 记账**、malloc/free 对象账、堆内切分策略、字节计费 |
| **Runtime**（`kcomp-sdk`，代码共享） | KernelNative：`GlobalAlloc` → Core 堆 ABI；私有执行域：per-instance `HeapState` + 分配器实现（Rust `GlobalAlloc` / C `malloc`），backing 经 `kcore_memory_acquire/release` | 拥有 backing、跨实例记账 |
| **Component** | 只写 `Vec` / `Box` / `malloc` / `free` | 知道 allocator 存在 |

判据（`AGENTS.md` 的 Core test）：`acquire` / `release` 进 Core，是因为**只有 Core 能**给全局 backing、只有 Core 能（Isolated 时）操作页表把 region 映射进实例 —— **不是因为要记账**。`kcore_heap_alloc/dealloc` 进 Core 是**部署形态**的结果：KernelNative 与 Core 同特权、同地址空间，共享的就是 Core 自己的堆（受信代码，不新造边界）；它只是这个既有分配器的窄 C ABI，**不**成为通用 / 跨域内存 ABI。

**两种堆后端（部署形态决定，不是组件的可选项）：**

- **KernelNative**：`GlobalAlloc` → `kcore_heap_alloc/dealloc` → Core `KernelAllocator`（slab + buddy）。同特权、同 AS，没有可裁决的隔离对象，因此**不记 owner、不设配额、失败不撤销**。
- **Isolated / Sandboxed**：私有分配器放在实例自己的可写 image（`.data` / `.bss`）里，backing 经 `kcore_memory_acquire/release` 以 region 粒度取得；**装载时显式拒绝** `kcore_heap_alloc/dealloc`（不静默回退）。

**"谁拥有哪段内存"这件事，Core 不记：**

- **KernelNative**：无隔离，记 owner 没有可裁决的对象，纯开销。
- **Isolated / Sandboxed**：**归属与映射由该实例的地址空间 / 页表承载**——Core 已经拥有那个 AS，页表就是记录，不另立账本。

普通 `malloc/free` 在 KernelNative 走共享 Core 堆 ABI（仍然不进任何 owner 账本）；私有执行域在同一 instance 已有的 region 里完成，只有 backing 不够时才向 Core 请求一次 memory resource。反过来，**Core 也不为 heap 记任何账**——`HeapState` 的内部账完全归 runtime。

## 2. Core ABI（域视图，无账本）

```c
/* kind 是整数常量，不用 Rust enum layout。 */
#define KCORE_MEMORY_VIEW_LOCAL_VA      1u
#define KCORE_MEMORY_VIEW_LINEAR_OFFSET 2u

typedef struct { uint32_t kind; uint32_t reserved; uint64_t base; uint64_t len; } kcore_memory_view;

int32_t kcore_memory_acquire(uint64_t min_len, uint64_t min_align, kcore_memory_view *out_view);
int32_t kcore_memory_release(const kcore_memory_view *view);
```

> `release` 传**指针**而非 by-value：RISC-V psABI 对 24B 聚合本来就按引用传参（wire 等价），
> 且 Core 的冻结 C 签名哨兵（`os/core/tests/kcomp_abi_drift.rs`）按宽度分类参数、没有
> by-value 聚合类别。语义以本节文字为准，"原样交回 acquire 给的 view" 不变。

- **无账本**：Core 不为 region 建记录、**不发 id**、**不记 owner**。`view` 自身（`base` / `len`）就是身份。
- **owner 不存在**：没有 owner 参数，也没有 owner 记录——本契约里"归属"不是 Core 的真相。
- **size / align**：`min_len > 0`、`min_align` 为非零 2 的幂；成功时 `view.len >= min_len`。实际 backing 粒度由 Core 决定（今天最小 4 KiB）。
- **view**：Native / Isolated 都给**本域 VA**；Sandboxed/WASM 给 linear-memory offset。**绝不**返回物理地址或 Core 私有 VA。
- **selection**：由 Core 的部署/后端决定，调用方不能请求 kind；不支持的组合返回 `-ENOTSUP`（不静默降级）。
- **contents**：首次交付**零初始化**。
- **failure**：返回 `-Errno`，不发布 backing、不改动 out。
- **release**：原样交回 `acquire` 给的 `view`。
  - KernelNative：**受信操作**（无额外鉴权）——`kcore_memory_release` 校验 `kind` / `reserved` 后直接 `free_region_raw(base, len)`，把 backing 归还分配器；**不触碰任何 AS**（本域没有映射可撤）。
  - Isolated：**组件可调用面未做**（§8；只读 / 诊断 import 面）。Core 内部的窗口回收（create / service 故障，`isolated_lifecycle::release_instance_windows`）先按**精确 acquire extent** 在 AS 里回找（`address_space::mapping_exact`）→ `unmap` 移除 PTE → 再用该 extent 的 physical range `free_region_raw` 归还 backing。
  - **移除 PTE ≠ 归还 backing**：`unmap` 只撤掉映射记录（AS 退役后不可再进入）；backing 只有拿着 AS 拥有的**精确 acquire extent** 才归还。release 依据不是"某个 PTE 存在"——别名映射下那样会猜错。
  - **不接受**调用方伪造的 base/len 当作释放依据：`free_region_raw` 只接受与 `alloc_region` 产物同形的 `(base, len)`（非零、页对齐、恰好落在某个 buddy order 上），否则 `EINVAL`；Isolated 的归属由页表兜住（`mapping_exact`），KernelNative 与既有 `dealloc` 同级，属受信边界。
- 用 `EINVAL` / `EOVERFLOW` / `ENOMEM` / `EPERM` / `ENOENT` / `ENOTSUP`，语义各自区分。

> **术语纠正**：native U-mode sandbox 仍然返回**本域 VA**；linear-memory offset 是 **WASM 执行后端**的属性，不是"沙箱特权"的属性（`deployment.md` 已把这两个维度正交）。

`out_view` 是**原生 C 传输的输出位置**，不是所取内存的表示。未来 syscall/host-call 传输必须把调用方输出位置 marshal 进 Core，**不得**把 guest 地址当 Core 指针解引用。

## 3. 访问窗口：语义统一，表示不统一

三种 ExecDomain **不共享 pointer representation**，只共享 **resource semantics**（"我可以拿到一块可访问内存"）：

| ExecDomain | 访问窗口 | 归属记录在哪 |
|---|---|---|
| KernelNative | 本域 VA（identity 映射下即物理基址） | **不记**（无隔离） |
| IsolatedNative | component-local VA（Core 私有 VA/物理 backing **不外泄**） | 该实例的 AS / 页表 |
| Sandboxed / WASM | linear-memory offset（由 sandbox runtime 自行管理） | sandbox runtime / WASM linear memory |

这正与 `kcore_device_claim` 的"本执行域访问窗口"同形。`MemoryLease` 仍是 **Core 内部 RAII**，不对外暴露。

## 4. 生命周期：没有账本，就没有"retire 表"

- **显式 release**：KernelNative → `kcore_memory_release` 直接归还 backing（本域无 AS 可撤）；Isolated 的组件可调用面**未做**（§8），Core 内部的窗口回收按 AS 的**精确 acquire extent** 走 `mapping_exact` → `unmap`（移除 PTE）→ `free_region_raw`（归还 backing）。**移除 PTE 只是撤映射，不等于归还 backing**——backing 的归还必须给出当初 acquire 的精确 extent，不能凭"某个 PTE 存在"或调用方伪造的 base/len（见 §2）。
- **instance 死亡**：
  - KernelNative → **无记录、不回收**。这正是"逻辑死亡、物理驻留"的结果；将来若要给 KernelNative 做物理回收，需要另立机制（那时才需要账本，不在本契约内）。
  - Isolated → **create / service 故障**（Core 中止实例）解映射并归还 Core 预置窗口 backing；**destroy 路径**（优雅停止或 destroy 入口故障）只退役 AS，窗口 backing 驻留（AS 退役后不可再进入）。页表页没有 teardown 接口，"不 leaked AS" = 退役后不再可达。
- **重启 = 重新 instantiate**：全新组件；KernelNative 共享 Core 堆（没有 per-instance 堆），私有执行域得到全新 `HeapState`，**绝不复用**失败堆。
- 优雅销毁可把私有对象还进本地 free list；但**不得**释放仍通过 Direct binding / 任务参数暴露的存储。

> **诚实边界**：KernelNative 的 release / failure 只保证**逻辑失效**，不承诺撤销裸指针或物理回收。真正的访问强制与安全复用依赖真实执行域（私有 AS + 页表）及 DMA 静默条件。

## 5. 私有执行域的 runtime context（不再有 runtime slot）

> 早期的 per-instance runtime slot / `tp` ambient 指针机制**已删除**：它没有生产消费方——KernelNative 的堆绑定是**静态后端选择**（Core 共享堆），不是 per-instance 指针；保留只会制造假前提。`tp` 回归普通架构 / 任务执行状态（Core 在任务切换 / trap 时透明保存 / 恢复，全新上下文起点为 0），不再是组件运行时身份，也不承载堆句柄。

- **KernelNative**：堆后端由部署形态静态选定（Core 共享堆），没有需要绑定的 per-instance 堆指针；`#[global_allocator]` 的 adapter static 天然 per-image（每次 instantiate 独立放段 / 重定位），但它只是适配器，不持有堆。
- **Isolated / Sandboxed**（目标）：私有分配器状态放在实例自己的可写 image backing 内；每次 instantiate 都独立按域放置 / 重定位，因此天然 per-instance，不需要 Core 侧的 slot 表。私有分配器经 `kcore_memory_acquire/release` 取 backing（§6）；该组件可调用面尚未实现（§8）。
- 组件的 create / task / Gate 入口与出口**不切换任何 ambient 堆指针**；执行边界只负责身份与 containment（见 `docs/architecture/component-lifecycle.md`）。

## 6. Runtime 分配器（部署后端）

- **KernelNative**：不新造分配器——直接共享 Core 的 `KernelAllocator`（同一 `HEAP` + `SLABS`，见 `os/core/src/memory/mod.rs`）。SDK 的 `GlobalAlloc` adapter（`kcomp-sdk/src/alloc.rs`）只做 ABI 转发：`alloc` / `dealloc` 传**原始** `(size, align)`，`realloc` = alloc + copy + dealloc（旧 Layout 原样交回）。契约 = Rust `GlobalAlloc`：`dealloc` 的 layout 必须与那次成功 alloc 逐字一致（共享堆按 `Layout` 路由 slab / buddy，错配 = UB，与 C `malloc/free` 同类）。接口本身不取 registry / endpoint / task 锁、不打印、不做 ownership / 记账 / 撤销。
- **Isolated / Sandboxed（目标，当前未接线）**：一份**私有 freestanding C 实现**（`kcomp-sdk/c/kalloc.c` + `include/kcomp_kalloc.h`，Rust facade 在 `kcomp-sdk/src/heap.rs`）：
  - 用**侵入式、可合并的 free list**，跨多段独立 acquire 的 region；bump-only 不适合 malloc/free。
  - 元数据放在 region 内部，增长不需要额外分配。
  - 对齐 / 溢出检查（含元数据开销）；耗尽返回 null；分配器**自己不得 panic、不得分配**。
  - 普通 `free` 把块还给**本 HeapState**，不还给 Core（只有整个 region 不再需要才 `release`）。
  - 初版**不支持 IRQ 上下文分配**（明确记录，避免同 CPU 自旋锁死锁）。
  - 该实现当前**保留但无生产调用方**（host 测试直接驱动真实 C 代码）；私有执行域落地时启用。它随 `.kcomp` 私有携带，**绝不**进 Core 导出白名单。
- "共享分配器实现代码" = **一份源码私有链进每个组件程序**，**不是**新建共享 Rust runtime，也不是把 allocator internals 变成 ABI。
- 增长可以几何式请求（128 → 256 → 512），但那是**请求容量**，不是物理占用承诺（今天最小一页）。

## 7. 明确不做

- **不做 Core 侧内存账本**：无 owner 记录、无 region 注册表、无 region id、无 Retired 表。
- **不做 per-instance 字节计费 / 配额**。
- **不把 `kcore_heap_alloc/dealloc` 当通用 / 跨域内存 ABI**：它只是 KernelNative 共享 Core 堆的部署后端；私有执行域装载时显式拒绝这两个符号（no silent fallback），未来的 Sandboxed / WASM 分配路径也不是它。
- **不把帧 / 区域分配**（`alloc_region` / `vm_page_alloc`）暴露给组件——组件取 backing 只经 `kcore_memory_acquire/release`；KernelNative 的普通堆分配走 heap ABI，不直取 region。
- **不为普通堆内存自动建立 DMA 依赖**：`resource/dma.rs` 记录的指针/范围**不构成**可安全释放的证明。启用物理复用前，先做 DMA 依赖/pinning 检查，或把 DMA 限定在专用 allocation 资源上。
- **不把 `ResourceDomain` 变成第二张表或通用资源图**。
- **不把每个 Core 内部 lease**（image 存储、Core 栈、页表页）翻成组件资源：它们保持既有内部资源。

## 8. 现状 / 目标

- **现状**：
  - **Memory resource**：`kcore_memory_acquire` / `kcore_memory_release`（`os/core/src/component/export.rs`）是 `memory::alloc_region` / `free_region_raw`（单一共享 buddy 堆 `MetadataHeap<32,12>`）上的薄 adapter，返回 / 接受 `kcore_memory_view` 域视图。组件面向的便利面是 SDK 的 `mem`（Rust）/ `kcomp_mem.h`（C）。`MemoryLease`（`os/core/src/memory/mod.rs`）是 Core 内部 region RAII，**无 owner 字段**；`alloc_region`/`free_region` 是 `pub(crate)`，刻意不在导出白名单。
  - **KernelNative heap**：`kcore_heap_alloc` / `kcore_heap_dealloc` 是既有 `memory::KernelAllocator`（slab + buddy）上的窄 C ABI，签名 / 语义由 `abi/core.toml` 单一来源生成；SDK feature `alloc` 的 `GlobalAlloc` adapter（`kcomp-sdk/src/alloc.rs`）直接走它。**不是**旧"通用共享堆 ABI"的复活：契约明确限定为 KernelNative 部署后端（Isolated / Sandboxed 装载显式拒绝）。
  - **Isolated / Sandboxed heap**：私有 freestanding C 分配器（`kcomp-sdk/c/kalloc.c` + `src/heap.rs` facade）已实现且 host 测试，但**未接线**（无生产调用方）；Isolated 组件当前只拿 Core 预置的实例窗口（以 `kcore_memory_view`（`LOCAL_VA`）编码预交付），组件可调用的 `kcore_memory_acquire/release` 面**未做**（不在 import 白名单里，装载前显式拒绝）。
- **目标**（本契约）：私有执行域在自己的可写 `.data` / `.bss` 里放置私有分配器，backing 经 `kcore_memory_acquire/release`；`kcore_memory_acquire/release` + `MemoryView` 保持**域感知 backing 机制**。Isolated 的归属由该实例的 AS / 页表承载，无隔离域不记归属；**没有** per-instance runtime slot / `tp` 堆指针（§5）。
> **Isolated 回收的诚实边界**：私有 backing 的**别名排除**保证 A 不能经 identity 看见 B 的 backing（`kernel_mappings::publish_private_backing` 对**所有活着的** root 逐条摘除 + 后续 root 的计划排除）；但页表页没有 teardown 接口，"退役 AS"只保证**不可再进入**，不承诺物理回收。create / service 故障解映射并归还预置窗口 extent；destroy 路径窗口驻留。KernelNative 无隔离，失败只是逻辑失效。

- 映射机制复用 `os/core/src/memory/address_space.rs`（`AddressSpaceManager` 已**有意重启**为 per-instance 表：`create_isolated_address_space_for`（共享 Core 映射 + 私有区）/ `map` / `unmap` / `mapping_exact` / `retire` / `prepare_activation` / `prepare_transition`，以及 `memory/kernel_mappings.rs` 的映射计划 + 私有 backing 别名排除事务，含 host 测试与 `Retired` 状态。私有 AS 切换机制在 `component/isolated.rs` + `arch/src/riscv/trampoline/`（最小 `satp` 切换汇编、普通 trap 路径往返、窄故障分派）；按域放段 / 逐段映射在 `component/isolated_load.rs`（页级权限分离 + 按域重定位 + 显式拒绝）；实例生命周期在 `component/isolated_lifecycle.rs`：私有 AS + 按域镜像 + **Core 预置的组件栈 / 实例内存窗口**（Core backing、零初始化、只映射在该实例的 AS 里）经 跨 AS trampoline 执行 `kcomp_instance_create` / `destroy` / `kcomp_service_dispatch`（service 的 caller 帧经共享 Core 映射直接交付——provider 原地读写 caller 缓冲，没有中间页；跨组件 transport 留给未来 I→I）。窗口生命周期两条路径：**create / service 故障 = Core 中止实例**（解映射并归还预置窗口 backing，半成品不留）；**destroy 路径**（入口成功或故障）只**退役 AS**，窗口 backing 保持驻留（phase 1 契约；AS 退役后不可再进入）。**重启 = 重新 instantiate**：每次全新按域放置到全新私有 backing / 全新 AS / 全新窗口（**没有** same-image backing 复用；同一 artifact 可并发多个组件；**没有** runtime slot 需要重建）。**内存路径决定**：`kcore_memory_acquire/release` 的组件可调用面**未做**（不在 import 白名单里，装载前显式拒绝）——Isolated 组件只拿 Core 预置的实例窗口（以 `kcore_memory_view`（`LOCAL_VA`）编码预交付；表示仍是**实例内 VA**，与本节 §3 的域视图同形），归属由该实例的页表承载。上述行为由 ArchTest 在 RV64/RV32 证明（`isolated-*` 系列）。boot 的单一 `RUNTIME_VM` 仍是独立真相，`adopt` hook 尚未接线——不要把它当现成的 per-component AS 执行路径）。
