# memory（os/core/src/memory/）

> Core 的**物理内存机制**：canonical 帧分配器（buddy）、Core 对象堆（仅 Core 内部）、区域 lease、以及地址空间的**语义词汇与所有权骨架**。堆（含组件侧）是 runtime / deployment 策略，不是 Core 资源，不在本模块（契约见 `docs/architecture/memory-and-heap.md`）。
> 记住三分法：Physical Memory（谁拥有哪些帧）/ Protection（谁能访问）/ Address Translation（VA→PA）——本模块管前者与词汇，翻译机制在 `arch`。

## owns 什么真相

- 物理帧真相与 canonical 分配器：`buddy_system_allocator::MetadataHeap`（O(1) buddy，metadata 自托管）。
- **早期内存 seam（Phase 4a）**：boot 做**无堆**内存 pass挑选唯一一段连续、页对齐、已排除镜像 / FDT / boot payload 等 live/reserved 区间的 RAM **arena**，经 `early_init(arena)` 一次性交给 Core；`core::init` **不再初始化 / 重置内存**，seam 未跑即 fail-closed。
- 受 Core 管理的区域归属（`MemoryLease` 是 Core 内部 RAII 独占 guard）。
- Core 对象堆仅供 Core 内部使用；组件侧堆是 runtime / deployment 策略（KernelNative 可共享 Core 内核堆，私有执行域可自带私有分配器）、**不由本模块拥有**。受管内存以 **region / address-space 粒度**提供 backing 与 mapping，**Core 不记 owner**（KernelNative 无账本；Isolated / Sandboxed 的归属由该实例的 AS / 页表承载），**不**记 malloc/free 对象、**不**做 per-instance 字节计费（见 `docs/architecture/memory-and-heap.md`）。
- 地址空间的语义真相：`KernelAddressSpace` 保存 mapping ledger；PTE 只是 backend 的硬件投影。

## 暴露什么机制

- `unsafe early_init(arena)`（一次性；形状 / 溢出 / 页对齐 / 最小容量校验 + alloc/free 探针 canary）与 `is_initialized()`；`select_arena(bank, image, scan)`（无堆：在包含镜像的 bank 内选最大页对齐间隙，排除集由 boot 逐条 emit，重复扫描、不缓存）与 `MIN_ARENA_SIZE`。
- `alloc_region(size) -> MemoryLease`；`free_region(lease)`；`vm_page_alloc() -> Result<usize, ()>`；`align_up_page`；`free_block_counts()`。（`init(region_start, region_end)` 仅存于 host 测试。）
- `KernelAllocator`（Core `GlobalAlloc`，KernelNative 经窄 heap ABI 共享；面向组件的内存面是 `kcore_memory_acquire/release`（域视图），**不引入 Core 侧账本**，见 `docs/architecture/memory-and-heap.md`）。
- 常量：`ALLOC_GRANULE = 4096`（物理分配粒度）、`HEAP_ORDER = 32`、`HEAP_MIN_ORDER = 12`。
- `MemoryLease`、`MemoryError`。
- `kcore_runtime_stats` 只读统计投影既有表；slab链表页/占用槽与Vec大对象buddy页分开计数，无新增owner或malloc账本。字段与并发观察边界见[内存契约 §9](../../architecture/memory-and-heap.md#9-runtime-回收矩阵目标与基线)。
- `address_space`：`KernelAddressSpace<B>`、`AddressSpaceManager<B>`、`AddressSpaceId`、`AddressSpaceHandle`、`AddressSpaceState`（`Ready` / `Retired`）、`Mapping`、`MapError`、`IsolatedPrepareError`、`PreparedActivation`（Core 内激活描述符，不经任何 `kcore_*` 导出）、`kernel_mappings`（共享 Core 映射计划 + 私有 backing 别名排除）；生命周期 API：`map` / `unmap` / `find_free_range`（VA 提议）/ `mapping_exact`（精确区间查询）/ `translate` / `prepare_activation` / `prepare_transition`（私有 AS 切换准备——校验入口 / 栈 + 落 激活准备 + 取描述符）/ `retire` / `adopt`（接管既有 backend 的 hook；boot root 尚未接线）；并重导出 `arch::vm::{AddressSpaceBackend, MappingPermission, PhysicalRange, VirtualRange}`。

## 明确不做

Runtime回收：retire 仅改状态；显式 reclaim 在证明 root/Task/inflight 静止后移除
AS 并归还 Sv39/Sv32 自有页表 frames。PageFree 不释放叶子 backing；私有独占 extents
由 component/reclaim 单独归还。unmap 不等于 free，K/DMA 与普通用户已发布 backing
不从此接口推导可回收。见[回收矩阵](../../architecture/memory-and-heap.md#9-runtime-回收矩阵目标与基线)。

- **不把帧 / 区域分配暴露给组件**：`alloc_region` / `vm_page_alloc` 不在导出白名单——物理分配是 Core 内部机制（`AGENTS.md`）。
- 不做内存记账：无 region owner 记录、无 region id、无 Retired 表，也不做 per-instance 字节计费 / 配额（`D1` 已修订；见 `docs/architecture/memory-and-heap.md`）。堆由 runtime / deployment 策略决定，不是 Core 记账。
- `alloc_region` **不负责清零**。
- `ALLOC_GRANULE` 与 `AddressSpaceBackend::GRANULE` 语义解耦（数值同为 4 KiB 只是巧合）。
- `AddressSpaceManager` 由 Core 的 Isolated 生命周期与 backing ABI 使用（`component/isolated_lifecycle.rs` 为实例建私有 AS、落按域镜像与 Core 预置窗口；`kcore_address_space_map` 刻意不在导出白名单）；boot 的长期 root 仍由 boot 的 `RuntimeVm` 持有（`adopt` hook 未接线）。`prepare_transition` + 最小跨 AS trampoline 与 `component/isolated_load.rs` 的按域放段 / 逐段映射由 `isolated_lifecycle.rs` 生产消费（create / destroy / service dispatch，含 Core 预置窗口 backing）；ArchTest 另直接驱动机制用例（`isolated-transition*` / `isolated-image*` / `isolated-lifecycle*` / `isolated-service*`）。失败 / 重启矩阵（`isolated-load-reject` / `isolated-config-reject` / `isolated-prepare-reject` / `isolated-destroy-fault` / `isolated-stale-access` / `isolated-ready-fault` / `isolated-restart`）把窗口生命周期钉成两条路径：create / service / destroy 失败先保留窗口，直到同一显式 reclaim 证明成立。

## 代码在哪

| 文件 | 内容 |
|---|---|
| `os/core/src/memory/mod.rs` | buddy heap、区域 API、`KernelAllocator`、`early_init` / `is_initialized` |
| `os/core/src/memory/early.rs` | 早期内存 seam：arena 形状校验、一次性启动、无堆 `select_arena` |
| `os/core/src/memory/address_space.rs` | 地址空间词汇 + 所有权骨架（`KernelAddressSpace` / manager） |
| `os/core/src/memory/slab.rs` | 小对象 slab 分配器 |
| `os/core/src/memory/test_support.rs` | host 测试初始化 / guard |

Isolated 动态 backing：`component/backing.rs` 在实例 AS 内选择空闲 VA、映射并排除共享 identity 别名；精确 release 先恢复共享别名再撤 owning mapping，完成后才归还物理 extent。root 创建与 backing 发布/释放共用 PLAN→SPACES 锁序。`isolated-heap` 验证同工件 K/I 堆后端、增长、release 与多实例隔离；停止后的堆 backing 先驻留，CPU-only 私有域显式 reclaim 后归还。
