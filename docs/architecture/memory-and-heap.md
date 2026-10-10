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
  - Isolated：只接受动态 backing 窗口（`0x23000000..0x2f000000`）里的**精确 acquire extent**：`mapping_exact` → 恢复共享 identity 别名（失败保留 owning mapping）→ `unmap` + 本地 TLB flush → `free_region_raw`。image / 栈 / ABI 窗口不能经组件 release 释放。调用方保证没有任务、回调或 DMA 继续借用。
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
| SandboxedNative（RV64 S/MMU） | 用户地址空间内 VA | 该实例的 AS / 页表 |
| WASM（后置） | linear-memory offset | WASM linear memory |

这正与 `kcore_device_claim` 的"本执行域访问窗口"同形。`MemoryLease` 仍是 **Core 内部 RAII**，不对外暴露。

## 4. 生命周期：没有账本，就没有"retire 表"

- **显式 release**：KernelNative 直接归还 backing；Isolated 按 AS 的精确动态 backing 映射走 `mapping_exact` → `unmap` → 恢复共享别名 → `free_region_raw`。Core 内部回收预置窗口也先验证 unmap 成功并恢复别名。**移除 PTE 只是撤映射，不等于归还 backing**；任一步失败都保留 backing，不能猜测后复用。
- **instance 死亡**：
  - KernelNative → **无记录、不回收**。这正是"逻辑死亡、物理驻留"的结果；将来若要给 KernelNative 做物理回收，需要另立机制（那时才需要账本，不在本契约内）。
  - I/U → 停止/失败先保留已发布 backing；CPU-only 显式 reclaim 在真实执行排空和全局私有域安全点后归还 image/heap/stack/页表。设备/DMA 不在本轮授权范围。
- **重启 = 重新 instantiate**：全新组件；KernelNative 共享 Core 堆（没有 per-instance 堆），私有执行域得到全新 `HeapState`，**绝不复用**失败堆。
- 优雅销毁可把私有对象还进本地 free list；但**不得**释放仍通过 Direct binding / 任务参数暴露的存储。

> **诚实边界**：KernelNative 的 release / failure 只保证**逻辑失效**，不承诺撤销裸指针或物理回收。真正的访问强制与安全复用依赖真实执行域（私有 AS + 页表）及 DMA 静默条件。

## 5. 私有执行域的 runtime context（不再有 runtime slot）

> 早期的 per-instance runtime slot / `tp` ambient 指针机制**已删除**：它没有生产消费方——KernelNative 的堆绑定是**静态后端选择**（Core 共享堆），不是 per-instance 指针；保留只会制造假前提。`tp` 回归普通架构 / 任务执行状态（Core 在任务切换 / trap 时透明保存 / 恢复，全新上下文起点为 0），不再是组件运行时身份，也不承载堆句柄。

- **KernelNative**：堆后端由部署形态静态选定（Core 共享堆），没有需要绑定的 per-instance 堆指针；`#[global_allocator]` 的 adapter static 天然 per-image（每次 instantiate 独立放段 / 重定位），但它只是适配器，不持有堆。
- **Isolated**：私有分配器状态在实例自己的可写 image backing 内；每次 instantiate 独立按域放置 / 重定位，因此天然 per-instance，不需要 Core slot。首次分配经 `kcore_memory_acquire` 放置 HeapState，后续仅增长时再请求 backing。RV64 Sandboxed 已复用相同 runtime，经 ecall 取得 USER backing。
- 组件的 create / task / Gate 入口与出口**不切换任何 ambient 堆指针**；执行边界只负责身份与 containment（见 `docs/architecture/component-lifecycle.md`）。

## 6. Runtime 分配器（部署后端）

- **KernelNative**：不新造分配器——直接共享 Core 的 `KernelAllocator`（同一 `HEAP` + `SLABS`，见 `os/core/src/memory/mod.rs`）。SDK 的 `GlobalAlloc` adapter（`kcomp-sdk/src/alloc.rs`）只做 ABI 转发：`alloc` / `dealloc` 传**原始** `(size, align)`，`realloc` = alloc + copy + dealloc（旧 Layout 原样交回）。契约 = Rust `GlobalAlloc`：`dealloc` 的 layout 必须与那次成功 alloc 逐字一致（共享堆按 `Layout` 路由 slab / buddy，错配 = UB，与 C `malloc/free` 同类）。接口本身不取 registry / endpoint / task 锁、不打印、不做 ownership / 记账 / 撤销。
- **Isolated / RV64 Sandboxed（已接线）**：一份**私有 freestanding C 实现**（`kcomp-sdk/c/kalloc.c` + `include/kcomp_kalloc.h`，Rust facade 在 `kcomp-sdk/src/heap.rs`）：
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
- **别名与复用**：发布私有 backing 时，从所有 Ready Isolated root 摘除其 identity 别名；以后创建的 root 也排除它。显式 release 先恢复原共享别名，再撤销精确私有映射并归还物理 extent。root 创建与排除/恢复共用事务锁，避免安装过期快照。每次跨 AS 进入仍用 ASID 0 + 全量 flush；私有 Task 可固定 CPU 运行；尚无通用远端 TLB shootdown，批量 reclaim 要求全局私有域安全点。
- **装载失败**：镜像的重定位、ABI 与入口校验全部通过后才发布私有 backing；校验失败直接归还未发布区域。发布可能部分移除别名，因此从发布尝试开始 image backing 就保持驻留，生产先声明 image owner 再发布，后续失败也不经 `Drop` 回收，可由该实例显式 reclaim。预置窗口先映射再发布；映射失败可直接归还，发布失败保留精确私有映射与 backing，让后续 reclaim 仍有所有权依据。
- **SandboxedNative**：RV64 S/MMU 已接 `.kcomp` loader / ecall / destroy；通过窄 ecall memory mechanism 获取 USER VA，其他目标返回 ENOTSUP；不得将 Core export 表或原生输出指针直接交给 U-mode。
- **验证**：host 测试驱动真实 C allocator / runtime；ArchTest `isolated-heap` 在 RV64/RV32 使用同一个 `kcomp_heap.kcomp` 验证 K/I 部署、Rust 与 C 分配、扩容、显式 backing release、并发实例互不干扰与全新实例重启。
- **回收边界**：普通 free 归还实例 free list。runtime 当前没有 region list，不自动归还整块空闲堆 backing；停止/失败先保留；CPU-only I/U 可以在显式 reclaim 中按既有 AS 精确映射归还独占 backing 和页表。DMA 静默仍未证明，K 共享堆不批量回收。

代码：`component/{load,loader,isolated_load,isolated_lifecycle,backing}.rs` 负责域装载与实例 backing；`memory/{address_space,kernel_mappings}.rs` 负责映射真相与别名事务；`kcomp-sdk/{src/alloc.rs,c/kcomp_heap_runtime.c,c/kalloc.c}` 负责业务无感的分配后端。

### 8.1 按需映射后置

当前 region 分配同步取得物理 backing 并映射，VA 的空闲区间查找只是地址放置机制。连续 VA 使用不连续物理页，以及 VA reserve / commit 可以独立演进，不要求先做 page fault。需要懒分配、按需栈增长、COW 或 pager 时，再增加 fault 机制与外置策略；不能将所有未映射地址或权限错误都当作堆增长请求。

## 9. Runtime 回收矩阵（目标与基线）

本节规定 CPU-only unload 的安全条件。K=KernelNative，I=IsolatedNative，
U=SandboxedNative（当前仅 RV64 S/MMU）。I/U 已有显式 reclaim，§4/§8 的 K 共享堆/
失败驻留语义不变；下表同时标注剩余缺口。
回收不变量：没有 CPU/保存现场继续使用，没有外借裸引用，没有设备继续访问风险，
且精确分配 extent 的所有权来源唯一，才可归还 buddy。unmap、Retired、Exited、
删除 ComponentRecord 均不单独构成证明。回收失败先保留并报告，不能猜测 free。

| 资源 / 获得时机 | 当前持有者与正常/失败处理 | K 回收条件 | I 回收条件 | U 回收条件 / 缺口 |
|---|---|---|---|---|
| text/rodata/data/bss：每次 ELF 放段 | `ComponentRecord.loaded.memory`；停止/失败先留驻；private reclaim 接管整份 allocation | 无 Task、返回地址、IRQ/policy/Gate 或外借 image 指针；否则保留 | owner 全部执行排空，所有段撤映射/flush/别名事务完成；一份 image lease 只释放一次 | 同 I；U USER 段/入口/装载已接 |
| 私有 AS：实例建立 | `AddressSpaceManager.spaces`；retire 只写状态；显式 reclaim 删除 AS 实体并释放页表 | 共享 Core root 永不按组件释放 | 所有 CPU 离开 root、保存 activation 失效；释放 backend 页表前确认 TLB 同步 | 同 I；复用普通 U-mode activation，不能让旧 sret 再进入 |
| 页表页：backend create/map | Sv39/Sv32 `frames`；`vm_page_alloc` forget lease；已有 PageFree/显式 backend teardown；unmap 本身不 free | 全局 root/共享映射常驻 | 在已排空 AS 中，后端拥有的 frames 逐页归还，不释放叶子指向的 backing | 同 I；page allocation 部分失败也需回收已分配表页 |
| 私有 heap backing：memory_acquire | 精确动态窗口 AS mapping；普通 free 只归 runtime free list；停止后驻留 | 共享 heap 无对象 owner 账，不能批量 free；显式 view release 为可信承诺 | 排空后按动态窗口精确 acquire extent 批量撤销；保留分配边界，不能按逐 PTE/任意 mapping free | 同 I；用户 copy 与 release 互斥，Core heap import 继续拒绝 |
| Task kernel stack/context：task_create | `TaskRecord.memory`/Box；remove 后 Drop 可 free，但生产 exit 没有 remove | 必须 incoming-stack 确认旧 context 保存完成、无 CPU/guard 再引用 | 同 K；Core 栈保持 Core root 可达，不当成私有业务栈 | 同 K；用户 trap 必须已返回该 task stack，再切离 |
| 组件 stack / ABI 窗口：I create 预置 | AS 精确映射；所有已发布失败/停止窗口先保留；同一 reclaim 证明后归还 | lifecycle 临时 Core stack 返回后可按 containment 路径处理，panic 放弃栈不猜测回收 | 私有 Task 需要每 Task 栈；lifecycle 栈保留到 destroy 返回；不能复用一张同步栈承载多个 Task | 同 I；U lifecycle runner 已接并受 existing inflight pin 保护 |
| 普通用户 stack/backing：user_map | `UserDomain.backing` lease；已发布 task 与 retired_user 留驻；Created staging discard 可释放 backing | personality owned user Task 有独立 AS，不能当共享 heap | 不作为 I 组件已经有栈的证据 | 复用 U 现场/copy；组件 AS 属于 Component，多 Task 不能各造第二实例 AS |
| Core runtime 对象：declare/publish/task | Registry/Endpoint tombstone、TaskRecord、全局 Exchange；记录长期驻留 | 保留身份最小 tombstone；Task 栈/ctx 在离场后释放，不因表清空称全部物理回收 | 同 K；AS retired 记录需拆除实体页表并保留失效身份 | 同 I；明确 slab 常驻页与 metadata 增长上界 |
| Endpoint/grant/request/reply：publish/listen/submit | Endpoint owner / Exchange Core 副本；exit/fail 关闭端口、移 grant、首终态保留 | 旧 Endpoint 永久失效；活 caller 已完成结果可延后 collect，不 pin provider image | 同 K；不把私有 buffer 指针存入 Exchange | 同 I；AS pin 覆盖输出验证、copy 和消费事务 |
| instance state：create 返回 | Core 只存 opaque 指针；destroy 由组件处理；failure 不 destroy | destroy 成功且无外借指针才可由组件 free；共享 heap 余留不可推断 | 无外部裸引用且执行排空后，随私有 backing 回收，不解释 malloc 对象 | 同 I；destroy 错误仍可条件性回收私有域 backing，不能称已业务清理 |
| IRQ/policy/其他 callback：注册/选择 | route / policy 栈及 registry inflight；撤 route 不等已取 callback 返回 | 阻止未来准入后等所有 callback 完成；未追踪外借 callback 则保留 | 初期不开放 IRQ/device；Gate/policy/lifecycle 仍需排空 | 初期不开放 native callback；未来走受控通知 |
| DMA allocation/mapping：alloc/map | `DmaTable` allocation lease / owner+device mapping；free/revoke 都进 Quarantine | 无设备静默证明一律保留；普通借入 buffer 未 pin，禁止据无 mapping 推断安全 | 本轮 import 继续拒绝；私有 backing 若可能外借给 DMA 同样不能回收 | CPU-only 首批不授权 DMA；未来 reset/IOMMU/pin 独立里程碑 |
| MMIO/device：claim | `DeviceTable` owner/quarantine；IRQ/DMA 子项未拆则 release EBUSY | 正常驱动静默后 release；异常设备隔离到 reboot，裸窗口协作撤销 | 初期不授权；不能因私有 unmap 忽略 identity MMIO 别名 | 初期不授权；未来受控映射且无 U 可达全局别名 |

### 9.1 最小拆除顺序与维护点

生命周期关闭准入 → Exchange 终结/通知 → Task 与 callback 离场确认 → 一次 destroy
（Force 跳过）→ 残余权限撤销 → 私有映射撤销和 TLB 确认 → 精确 backing 与页表页归还。
这是依赖关系；设备静默必须在对应 DMA free 之前，destroy 所需 image/stack 最后释放。

复用 `LoadedComponent.memory` 保留整个 allocation：I 各段映射只是同一 image lease
的切片，不能逐段 free。Task lease 由 TaskRecord 拆除；动态 backing 由现有 AS 精确
窗口映射枚举，不复制成 per-instance heap region 表。页表用现有 frames；已增加最窄
PageFree/teardown seam 和未发布 root 回滚纪律，不再引入 allocator policy 或共享 runtime。

私有 mapping 不一定拥有 backing：共享 Core 映射、image 切片、未来共享 buffer 均不
能用通用“unmap 所有 mapping 再 free”处理。动态窗口必须维持 acquire extent 边界，
若权限编辑拆分了它，需要先恢复可靠的 extent 依据；不能把 PTE 数量当分配数量。

当前 `release_private_backing` 对多个 live root 恢复 identity 别名；reclaim
要求全局私有域无 Running Task/生命周期执行，按 registry 锁阻止新进入。显式动态
release 仍由可信组件保证无继续借用；泛化跨 CPU mapping 编辑需后续远端 invalidation。不仅停止目标 root，
还要考虑 publish/release 会修改仍在别的 CPU 运行的 root。优先固定 CPU 的阶段性约束，
不能让恢复别名后马上复用物理页早于必要确认。

基线诊断不足：`fail_component` 丢弃 reason，窗口 cleanup best-effort 结果不报告，
DMA QUARANTINE 只有 lease、无完整公开保留理由。目标在既有记录/trace 增补原因与
extent/数量，不建立 Retired 资源数据库。已完成 IPC 副本可独立留在 Core，允许存活
caller 在 provider backing 回收后 collect；它不应要求 provider image 继续驻留。

验收按资源分层：逻辑活跃数量、精确 backing extent、可用物理页、页表页、保留原因、
预期 tombstone/缓存分别计数。允许 Core slab 常驻缓存，但每个新增保留必须有归属与
界限。1000 轮 host Exchange 只验证槽位。另有真实 I/U 1000 轮 reclaim，逐轮 Task 数恢复且
可用物理页增加。首批保留192页尚未归因；后续计量补丁和最新证据见
[Runtime报告 §8](../development/component-runtime-consolidation.md#8-回收计量补丁在87b86be之后)。

只读观察入口 `kcore_runtime_stats`（布局唯一来源 `abi/core.toml`）投影既有
Registry/Endpoint/Task/AS/Exchange/共享映射计划与slab。不建立内存owner账本，
不发资源身份，不记录malloc事件；仅live KernelNative管理上下文可调用。
`metadata_pages`是各Vec容量按现有buddy layout分配形状取整得到的独占页；
small object共享页统一计入`slab_pages`，不能按各名称分别向上取整再重复相加。
`metadata_slab_objects/bytes`只投影上述元数据Vec的小对象槽，不含独立名称；
用于区分Vec扩容从slab进入buddy时的旧槽释放，已经包含在全局slab数量中。
`private_mapping_pages`只描述私有映射长度，不等于唯一物理backing量；
`page_table_pages`只数现有AS backend的独占frames，不含boot自持root或叶子backing。
子表分别取锁，无动态分配；并发时不是全系统原子快照，不能用于回收判定。
受控压力在各可回收资源回基线后核对free页减少是否严格等于metadata/slab页增长；
非零差额必须失败并报告，不能解释为测试误差。一般并发/DMA等场景仍需各自证明。
