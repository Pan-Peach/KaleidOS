# memory（os/core/src/memory/）

> Core 的**物理内存机制**：canonical 帧分配器（buddy）、Core 堆（共享）、区域 lease、以及地址空间的**语义词汇与所有权骨架**。
> 记住三分法：Physical Memory（谁拥有哪些帧）/ Protection（谁能访问）/ Address Translation（VA→PA）——本模块管前者与词汇，翻译机制在 `arch`。

## owns 什么真相

- 物理帧真相与 canonical 分配器：`buddy_system_allocator::MetadataHeap`（O(1) buddy，metadata 自托管），区域 `[align_up(__bootstrap_end), RAM 末尾)`。
- 受 Core 管理的区域归属（`MemoryLease` 是 Core 内部 RAII 独占 guard）。
- Core 与组件**共享一个 Core heap**（无 per-component 记账、无 per-component arena）。
- 地址空间的语义真相：`KernelAddressSpace` 保存 mapping ledger；PTE 只是 backend 的硬件投影。

## 暴露什么机制

- `init(region_start, region_end)`；`alloc_region(size) -> MemoryLease`；`free_region(lease)`；`vm_page_alloc() -> Result<usize, ()>`；`align_up_page`；`free_block_counts()`。
- `KernelAllocator`（`GlobalAlloc`，接 Core 共享堆）。
- 常量：`ALLOC_GRANULE = 4096`（物理分配粒度）、`HEAP_ORDER = 32`、`HEAP_MIN_ORDER = 12`。
- `MemoryLease`、`MemoryError`。
- `address_space`：`KernelAddressSpace<B>`、`AddressSpaceManager<B>`、`AddressSpaceId`、`AddressSpaceHandle`、`Mapping`、`MapError`，并重导出 `arch::vm::{AddressSpaceBackend, MappingPermission, PhysicalRange, VirtualRange}`。

## 明确不做

- **不把帧 / 区域分配暴露给组件**：`alloc_region` / `vm_page_alloc` 不在导出白名单——物理分配是 Core 内部机制（`AGENTS.md`）。
- 不做 per-component 字节计费 / 私有堆（`D1=A`）。
- `alloc_region` **不负责清零**。
- `ALLOC_GRANULE` 与 `AddressSpaceBackend::GRANULE` 语义解耦（数值同为 4 KiB 只是巧合）。
- `AddressSpaceManager` 生产路径**从未实例化**（休眠）；`kcore_address_space_map` 刻意不在导出白名单。

## 代码在哪

| 文件 | 内容 |
|---|---|
| `os/core/src/memory/mod.rs` | buddy heap、区域 API、`KernelAllocator` |
| `os/core/src/memory/address_space.rs` | 地址空间词汇 + 所有权骨架（`KernelAddressSpace` / manager） |
| `os/core/src/memory/slab.rs` | 小对象 slab 分配器 |
| `os/core/src/memory/test_support.rs` | host 测试初始化 / guard |
