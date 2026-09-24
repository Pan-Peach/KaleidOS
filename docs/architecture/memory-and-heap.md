# memory 与 heap：Core Memory ↔ Runtime Heap

> 本文件是**内存资源（Core）**与**堆（运行时）**的权威契约。
> 一句话：**Core 管 Memory，不管 Heap，也不做内存记账。**
> 与 `AGENTS.md` 的不可违背原则一致；访问窗口与 `driver-model.md` 的 device claim **同形**——两者都返回「本执行域访问窗口」。

## 1. 分层

| 层 | 负责 | 不负责 |
|---|---|---|
| **Core** | Memory：backing / mapping（按需给 backing；Isolated 时把 region 映射进该实例的 AS） | **owner 记账**、malloc/free 对象、堆内切分、字节计费 |
| **Runtime**（`kcomp-sdk`，代码共享） | per-instance `HeapState` + 分配器实现（Rust `GlobalAlloc` / C `malloc`） | 拥有 backing、跨实例记账 |
| **Component** | 只写 `Vec` / `Box` / `malloc` / `free` | 知道 allocator 存在 |

判据（`AGENTS.md` 的 Core test）：`acquire` / `release` 进 Core，是因为**只有 Core 能**给全局 backing、只有 Core 能（Isolated 时）操作页表把 region 映射进实例 —— **不是因为要记账**。

**"谁拥有哪段内存"这件事，Core 不记：**

- **KernelNative**：无隔离，记 owner 没有可裁决的对象，纯开销。
- **Isolated / Sandboxed**：**归属与映射由该实例的地址空间 / 页表承载**——Core 已经拥有那个 AS，页表就是记录，不另立账本。

普通 `malloc/free` **不进 Core**：在同一 instance 已有的 region 里完成；只有 backing 不够时才向 Core 请求一次 memory resource。反过来，**Core 也不为 heap 记任何账**——`HeapState` 的内部账完全归 runtime。

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
  - KernelNative：**受信操作**（无额外鉴权）——把 backing 归还给分配器。
  - Isolated：由该实例的 AS 校验（页表就是记录），解映射 + 归还。
  - **不接受**调用方伪造的 base/len 当作释放依据（Isolated 由页表兜住；KernelNative 与既有 `dealloc` 同级，属受信边界）。
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

- **显式 release**：KernelNative → 归还 backing；Isolated → 解映射 + 归还。
- **instance 死亡**：
  - KernelNative → **无记录、不回收**。这正是"逻辑死亡、物理驻留"的结果；将来若要给 KernelNative 做物理回收，需要另立机制（那时才需要账本，不在本契约内）。
  - Isolated → **create / service 故障**（Core 中止实例）解映射并归还 Core 预置窗口 backing；**destroy 路径**（优雅停止或 destroy 入口故障）只退役 AS，窗口 backing 驻留（AS 退役后不可再进入）。页表页没有 teardown 接口，"不 leaked AS" = 退役后不再可达。
- **逻辑重启** = 全新 instance、全新 `HeapState`，**绝不复用**失败堆。
- 优雅销毁可把私有对象还进本地 free list；但**不得**释放仍通过 Direct binding / 任务参数暴露的存储。

> **诚实边界**：KernelNative 的 release / failure 只保证**逻辑失效**，不承诺撤销裸指针或物理回收。真正的访问强制与安全复用依赖真实执行域（私有 AS + 页表）及 DMA 静默条件。

## 5. 前置：per-instance runtime context（不可跳过）

> **这是本契约能成立的前提**，不是后续优化。

`component/image.rs` 的 `ImageTable` 按 artifact 名**复用 image**，所以 image 的可写 static（含 `#[global_allocator]` 的内部状态）**不是** per-instance 存储。把 `CoreHeap` 换成 SDK static allocator 会得到一个 **image-global** 堆，违反本契约。

- Core 为每个 instance 关联一个**稳定的 runtime slot**（内含 runtime 自有的 opaque 状态指针；Core **从不解释**它）。
- 组件每个入口（create、task 切换、Gate 进入、**Direct provider 入口**、panic escape）都 **建立**正确的 runtime context；每个出口 / 非局部逃逸都 **恢复**。
- Direct **不切 ambient 归属**（`deployment.md`），所以 Direct 的 SDK adapter 必须在调用 provider 前切到 provider 的**已注册 slot**。
- 这是**协作式 KernelNative 记账，不是鉴权隔离**：slot 切换不得开启跨域访问，也不得改变"panic 归属哪个 containment 边界"。

**不要**引入编译器 TLS 重定位，再把 `.kcomp` loader 变成 TLS linker。最小实现是**一个执行上下文寄存器**（RISC-V 上 `tp`；psABI 标记 `tp` 为 unallocatable/固定，编译器永不分配或写入它），由 trap/切换路径显式 save/restore。窄契约放 `abi/component.toml`，运行时访问与 bootstrap 放一个小 SDK runtime module。

**bootstrap 不得递归分配**：首次进入 → 装好 slot（此时无堆指针）→ 直接 `kcore_memory_acquire` → 把 `HeapState` 与初始 region 元数据**放进这块 backing** → 发布 opaque 堆指针 → 才调用应用代码。

## 6. Runtime 分配器（共享代码，非共享堆）

- 用**侵入式、可合并的 free list，跨多段独立 acquire 的 region**。bump-only 不适合 malloc/free。
- 元数据放在 region 内部，增长不需要额外分配。
- 对齐 / 溢出检查（含元数据开销）；耗尽返回 null；分配器**自己不得 panic、不得分配**。
- 普通 `free` 把块还给**本 HeapState**，不还给 Core（只有整个 region 不再需要才 `release`）。
- 初版**不支持 IRQ 上下文分配**（明确记录，避免同 CPU 自旋锁死锁）。
- **一份私有 freestanding C 实现**（`kcomp-sdk/c/`），Rust `GlobalAlloc` 只是它的 adapter。其函数**只链进各 `.kcomp`**，**绝不**进 Core 导出白名单。
- "共享分配器代码" = **一份源码私有链进每个组件程序**，**不是**新建共享 Rust runtime，也不是把 allocator internals 变成 ABI。
- 增长可以几何式请求（128 → 256 → 512），但那是**请求容量**，不是物理占用承诺（今天最小一页）。

## 7. 明确不做

- **不做 Core 侧内存账本**：无 owner 记录、无 region 注册表、无 region id、无 Retired 表。
- **不做 per-instance 字节计费 / 配额**。
- **不把帧 / 区域分配**（`alloc_region` / `vm_page_alloc`）暴露给组件——组件只经 `kcore_memory_acquire/release`。
- **不为普通堆内存自动建立 DMA 依赖**：`resource/dma.rs` 记录的指针/范围**不构成**可安全释放的证明。启用物理复用前，先做 DMA 依赖/pinning 检查，或把 DMA 限定在专用 allocation 资源上。
- **不把 `ResourceDomain` 变成第二张表或通用资源图**。
- **不把每个 Core 内部 lease**（image 存储、Core 栈、页表页）翻成组件资源：它们保持既有内部资源。

## 8. 现状 / 目标

- **现状**：`kcore_memory_acquire` / `kcore_memory_release`（`os/core/src/component/export.rs`）是 `memory::alloc_region` / `free_region_raw`（单一共享 buddy 堆 `MetadataHeap<32,12>`）上的薄 adapter，返回 / 接受 `kcore_memory_view` 域视图；旧的共享堆 `kcore_heap_alloc/dealloc` 已**原地删除**（无别名、无 legacy fallback）。组件面向的便利面是 SDK 的 `mem`（Rust）/ `kcomp_mem.h`（C）。`MemoryLease`（`os/core/src/memory/mod.rs`）是 Core 内部 region RAII，**无 owner 字段**；`alloc_region`/`free_region` 是 `pub(crate)`，刻意不在导出白名单。
- **目标**（本契约）：SDK runtime 提供 per-instance `HeapState`（`kcomp-sdk` 的 `heap` / `alloc`）；Isolated 的归属由该实例的 AS / 页表承载，无隔离域不记归属。
- **先行条件**：§5 的 per-instance runtime context（`tp`）。
- 映射机制复用 `os/core/src/memory/address_space.rs`（`AddressSpaceManager` 已**有意重启**为 per-instance 表：`create_address_space_for` / `map` / `unmap` / `mapping_exact` / `retire` / `prepare_activation` / `prepare_transition`，含 host 测试与 `Retired` 状态。**increment 3** 落地的私有 AS 切换机制在 `component/isolated.rs` + `arch/src/riscv/gateway/`（双映射汇编、trap 往返、窄故障分派）；**increment 4** 落地的按域放段 / 逐段映射在 `component/isolated_load.rs`（页级权限分离 + 按域重定位 + 显式拒绝）；**increment 5** 的实例生命周期在 `component/isolated_lifecycle.rs`：私有 AS + 按域镜像 + **Core 预置的组件栈 / 实例内存窗口**（Core backing、零初始化、只映射在该实例的 AS 里）经 assembly gateway 执行 `kcomp_instance_create` / `destroy`，失败即退役 AS + 归还窗口；**increment 6** 再预置一页**服务邮箱**（同一 backing 纪律）承载跨 AS 的扁平调用帧拷贝（`component/isolated_mailbox.rs`：caller args / input → 邮箱 → provider 域内 VA，output 拷回 caller），并把 `kcomp_service_dispatch` 接入同一 gateway。**increment 7** 的失败 / 重启矩阵把窗口生命周期钉死为两条路径：**create / service 故障 = Core 中止实例**（解映射并归还预置窗口 backing，半成品不留）；**destroy 路径**（入口成功或故障）只**退役 AS**，窗口 backing 保持驻留（phase 1 契约；AS 退役后不可再进入）。**逻辑重启**复用 image 常驻 backing（同域 image 复用，全新 AS / 全新窗口 / 全新 slot；并发活跃实例显式拒绝），失败实例的窗口要么已归还、要么随退役 AS 一起不可达。**内存路径决定**：`kcore_memory_acquire/release` 的组件可调用面（component→Core gate-call trampoline）**未做**——Isolated 组件只拿 Core 预置的实例窗口（以 `kcore_memory_view`（`LOCAL_VA`）编码预交付；表示仍是**实例内 VA**，与本节 §3 的域视图同形），归属由该实例的页表承载。上面全部由 ArchTest 在 RV64/RV32 证明（`isolated-*` / `isolated-lifecycle*` / `isolated-service*` / `isolated-load-reject` / `isolated-config-reject` / `isolated-prepare-reject` / `isolated-destroy-fault` / `isolated-stale-access` / `isolated-ready-fault` / `isolated-restart`）。boot 的单一 `RUNTIME_VM` 仍是独立真相，`adopt` hook 尚未接线——不要把它当现成的 per-component AS 执行路径）。
