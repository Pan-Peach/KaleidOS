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
- **view**：native 后端的 KernelNative / Isolated / Sandboxed 都给**本域 VA**；只有 WASM 后端给 linear-memory offset。私有执行域**绝不**返回物理地址或 Core 私有 VA。
- **selection**：由 Core 的部署/后端决定，调用方不能请求 kind；不支持的组合返回 `-ENOTSUP`（不静默降级）。
- **contents**：首次交付**零初始化**。
- **failure**：返回 `-Errno`，不发布 backing、不改动 out。
- **release**：原样交回 `acquire` 给的 `view`。
  - KernelNative：**受信操作**（无额外鉴权）——`kcore_memory_release` 校验 `kind` / `reserved` 后直接 `free_region_raw(base, len)`，把 backing 归还分配器；**不触碰任何 AS**（本域没有映射可撤）。
  - Isolated：只接受动态 backing 窗口（`0x23000000..0x2f000000`）里的**精确 acquire extent**：`mapping_exact` → `unmap` + 本地 TLB flush → 恢复共享 identity 别名 → `free_region_raw`。image / 栈 / ABI 窗口不能经组件 release 释放。调用方保证没有任务、回调或 DMA 继续借用。
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
| SandboxedNative（后置） | 用户地址空间内 VA | 该实例的 AS / 页表 |
| WASM（后置） | linear-memory offset | WASM linear memory |

这正与 `kcore_device_claim` 的"本执行域访问窗口"同形。`MemoryLease` 仍是 **Core 内部 RAII**，不对外暴露。

## 4. 生命周期：没有账本，就没有"retire 表"

- **显式 release**：KernelNative 直接归还 backing；Isolated 按 AS 的精确动态 backing 映射走 `mapping_exact` → `unmap` → 恢复共享别名 → `free_region_raw`。Core 内部回收预置窗口也先验证 unmap 成功并恢复别名。**移除 PTE 只是撤映射，不等于归还 backing**；任一步失败都保留 backing，不能猜测后复用。
- **instance 死亡**：
  - KernelNative → **无记录、不回收**。这正是"逻辑死亡、物理驻留"的结果；将来若要给 KernelNative 做物理回收，需要另立机制（那时才需要账本，不在本契约内）。
  - Isolated → **create / service 故障**解映射并归还 Core 预置窗口 backing；**destroy 路径**只退役 AS，窗口 backing 驻留。image、私有堆 backing 与页表页当前保持驻留，停止后不自动批量归还；完整回收需要 drain / DMA 静默与 AS teardown。
- **重启 = 重新 instantiate**：全新组件；KernelNative 共享 Core 堆（没有 per-instance 堆），私有执行域得到全新 `HeapState`，**绝不复用**失败堆。
- 优雅销毁可把私有对象还进本地 free list；但**不得**释放仍通过 Direct binding / 任务参数暴露的存储。

> **诚实边界**：KernelNative 的 release / failure 只保证**逻辑失效**，不承诺撤销裸指针或物理回收。真正的访问强制与安全复用依赖真实执行域（私有 AS + 页表）及 DMA 静默条件。

## 5. 私有执行域的 runtime context（不再有 runtime slot）

> 早期的 per-instance runtime slot / `tp` ambient 指针机制**已删除**：它没有生产消费方——KernelNative 的堆绑定是**静态后端选择**（Core 共享堆），不是 per-instance 指针；保留只会制造假前提。`tp` 回归普通架构 / 任务执行状态（Core 在任务切换 / trap 时透明保存 / 恢复，全新上下文起点为 0），不再是组件运行时身份，也不承载堆句柄。

- **KernelNative**：堆后端由部署形态静态选定（Core 共享堆），没有需要绑定的 per-instance 堆指针；`#[global_allocator]` 的 adapter static 天然 per-image（每次 instantiate 独立放段 / 重定位），但它只是适配器，不持有堆。
- **Isolated**：私有分配器状态在实例自己的可写 image backing 内；每次 instantiate 独立按域放置 / 重定位，因此天然 per-instance，不需要 Core slot。首次分配经 `kcore_memory_acquire` 放置 HeapState，后续仅增长时再请求 backing。Sandboxed 的相同 runtime 选择有 host 验证，组件执行后端后置。
- 组件的 create / task / Gate 入口与出口**不切换任何 ambient 堆指针**；执行边界只负责身份与 containment（见 `docs/architecture/component-lifecycle.md`）。

## 6. Runtime 分配器（部署后端）

- **KernelNative**：不新造分配器——直接共享 Core 的 `KernelAllocator`（同一 `HEAP` + `SLABS`，见 `os/core/src/memory/mod.rs`）。SDK 的 `GlobalAlloc` adapter（`kcomp-sdk/src/alloc.rs`）只做 ABI 转发：`alloc` / `dealloc` 传**原始** `(size, align)`，`realloc` = alloc + copy + dealloc（旧 Layout 原样交回）。契约 = Rust `GlobalAlloc`：`dealloc` 的 layout 必须与那次成功 alloc 逐字一致（共享堆按 `Layout` 路由 slab / buddy，错配 = UB，与 C `malloc/free` 同类）。接口本身不取 registry / endpoint / task 锁、不打印、不做 ownership / 记账 / 撤销。
- **Isolated（已接线）/ Sandboxed（执行后端后置）**：一份**私有 freestanding C 实现**（`kcomp-sdk/c/kalloc.c` + `include/kcomp_kalloc.h`，Rust facade 在 `kcomp-sdk/src/heap.rs`）：
  - 用**侵入式、可合并的 free list**，跨多段独立 acquire 的 region；bump-only 不适合 malloc/free。
  - 元数据放在 region 内部，增长不需要额外分配。
  - 对齐 / 溢出检查（含元数据开销）；耗尽返回 null；分配器**自己不得 panic、不得分配**。
  - 普通 `free` 把块还给**本 HeapState**，不还给 Core（只有整个 region 不再需要才 `release`）。
  - 初版**不支持 IRQ 上下文分配**（明确记录，避免同 CPU 自旋锁死锁）。
  - `kcomp_heap_runtime.c` 的 per-image 部署 adapter 消费它；Rust `ComponentHeap` 与 C 的 weak `malloc/free/calloc/realloc` 共用 adapter。它随 `.kcomp` 私有携带，**绝不**进 Core 导出白名单。
- "共享分配器实现代码" = **一份源码私有链进每个组件程序**，**不是**新建共享 Rust runtime，也不是把 allocator internals 变成 ABI。
- 增长可以几何式请求（128 → 256 → 512），但那是**请求容量**，不是物理占用承诺（今天最小一页）。

### 6.1 同一工件的部署选择

SDK 可选导出 `kcomp_runtime_init(const struct kcomp_runtime *)`；loader 保留并校验这个入口，Core 在业务 create 前以实例身份调用一次。描述符只有整数 domain、reserved 与两项窄 C ABI 地址：KernelNative 交付共享堆 alloc/dealloc，Isolated / Sandboxed 两项必须为零。SDK 将选择存入本镜像的 `.bss`；业务代码仍写 `Vec` / `Box` / `malloc`，没有执行域分支，也不直接 import `kcore_heap_*`。

Isolated 初始化入口与 create 一样经私有 AS trampoline 调用，描述符放在 ABI 窗口 `+352`；初始化失败按 create 失败处理。没有 SDK heap 的组件可以省略入口。没有 `tp` 堆指针、共享 Rust runtime 或跨域 allocator internals。

## 7. 明确不做

- **不做 Core 侧内存账本**：无 owner 记录、无 region 注册表、无 region id、无 Retired 表。
- **不做 per-instance 字节计费 / 配额**。
- **不把 `kcore_heap_alloc/dealloc` 当通用 / 跨域内存 ABI**：它只是 KernelNative 共享 Core 堆的部署后端；私有执行域装载时显式拒绝这两个符号（no silent fallback），未来的 Sandboxed / WASM 分配路径也不是它。
- **不把帧 / 区域分配**（`alloc_region` / `vm_page_alloc`）暴露给组件——组件取 backing 只经 `kcore_memory_acquire/release`；KernelNative 的普通堆分配走 heap ABI，不直取 region。
- **不为普通堆内存自动建立 DMA 依赖**：`resource/dma.rs` 记录的指针/范围**不构成**可安全释放的证明。启用物理复用前，先做 DMA 依赖/pinning 检查，或把 DMA 限定在专用 allocation 资源上。
- **不把 `ResourceDomain` 变成第二张表或通用资源图**。
- **不把每个 Core 内部 lease**（image 存储、Core 栈、页表页）翻成组件资源：它们保持既有内部资源。

## 8. 现状 / 目标

- **KernelNative**：runtime 初始化取得 Core 共享堆窄 C ABI；普通对象分配无 owner 账本。显式 backing acquire/release 仍是受信 region 操作。
- **IsolatedNative**：支持 `kcore_memory_acquire/release` import；Core 从调用身份选择实例 AS，在动态 backing 窗口提议空闲 VA 并重新验证映射。SDK 在实例内放置、增长和串行访问私有堆。Core 只存 region 映射，不记录 malloc 对象。
- **别名与复用**：发布私有 backing 时，从所有 Ready Isolated root 摘除其 identity 别名；以后创建的 root 也排除它。显式 release 撤销精确私有映射、恢复原共享别名后才归还物理 extent。root 创建与排除/恢复共用事务锁，避免安装过期快照。每次跨 AS 进入仍用 ASID 0 + 全量 flush；尚无私有 AS 的 SMP 任务调度或通用远端 TLB shootdown。
- **SandboxedNative**：runtime 私有分配器选择有 host 测试；组件 loader / ecall / destroy 仍未实现，创建显式 `-ENOTSUP`。未来通过窄 ecall memory mechanism 获取用户 VA；不得将 Core export 表或原生输出指针直接交给 U-mode。
- **验证**：host 测试驱动真实 C allocator / runtime；ArchTest `isolated-heap` 在 RV64/RV32 使用同一个 `kcomp_heap.kcomp` 验证 K/I 部署、Rust 与 C 分配、扩容、显式 backing release、并发实例互不干扰与全新实例重启。
- **回收边界**：普通 free 归还实例 free list。runtime 当前没有 region list，不自动归还整块空闲堆 backing；停止/失败后的 image、heap backing 与页表页保持驻留。完整物理回收留给 drain / DMA 静默和 AS teardown。

代码：`component/{load,loader,isolated_load,isolated_lifecycle,backing}.rs` 负责域装载与实例 backing；`memory/{address_space,kernel_mappings}.rs` 负责映射真相与别名事务；`kcomp-sdk/{src/alloc.rs,c/kcomp_heap_runtime.c,c/kalloc.c}` 负责业务无感的分配后端。

### 8.1 按需映射后置

当前 region 分配同步取得物理 backing 并映射，VA 的空闲区间查找只是地址放置机制。连续 VA 使用不连续物理页，以及 VA reserve / commit 可以独立演进，不要求先做 page fault。需要懒分配、按需栈增长、COW 或 pager 时，再增加 fault 机制与外置策略；不能将所有未映射地址或权限错误都当作堆增长请求。
