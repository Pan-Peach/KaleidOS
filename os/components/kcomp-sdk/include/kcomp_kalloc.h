/* kcomp_kalloc.h —— 私有执行域运行时堆的 C 接口（freestanding，无 libc）。
 *
 * **保留为未来 Isolated / Sandboxed 的私有分配器后端**（当前没有生产调用方）：
 * KernelNative 组件与 Core 同特权、同地址空间，普通 malloc/free 直接走
 * `kcore_heap_alloc` / `kcore_heap_dealloc`（共享 Core 堆，契约 = Rust
 * `GlobalAlloc`）；私有执行域才在实例自己的可写 `.data` / `.bss` 里放本分配器。
 * 契约：docs/architecture/memory-and-heap.md §6。
 *
 * 分配器实现是 **C**（`c/kalloc.c`），随每个 `.kcomp` 私有携带（"共享分配器
 * 实现代码，不是共享堆"）；Rust 侧 facade 见 `src/heap.rs`。
 *
 * 用法（未来私有域 bootstrap）：
 *   region = kcore_memory_acquire(...)            // 先有 backing，才发布堆
 *   heap   = kcomp_heap_place(region.base, region.len, backing_fn)
 *   ptr    = kcomp_heap_alloc(heap, size, align)  // 普通 malloc 不再进 Core
 *
 * 语义：
 *   - `backing` 与 `kcore_memory_acquire` 同形（1:1）：成功 = 0，失败 = `-Errno`。
 *     分配器**只在 free list 放不下时**才调用它，且是几何式请求容量。
 *   - 元数据（堆头 / 块头）全部在 region 内部；增长不产生任何辅助分配。
 *   - 失败一律返回 NULL，分配器自己**绝不 panic、绝不分配**（size / align
 *     溢出与非 2 的幂 align 都在入口拒绝）。
 *   - `kcomp_heap_realloc` 失败返回 NULL 且**旧块原样保留**（旧指针仍可 free）；
 *     `size == 0` 视为失败（no-op），不释放旧块。
 *   - 只导出 `kcomp_heap_*`；**不**定义 malloc / free / calloc / realloc，
 *     避免与 `kcomp_rt.c` / picolibc 之类的提供者冲突。
 *
 * v1 边界（明确记录，别当成 bug）：
 *   - **不支持 IRQ 上下文分配**：v1 无锁；将来若加单 CPU 自旋锁，中断里分配
 *     会自死锁。
 *   - **没有 region list**：整段 region release 不在 v1 范围内（Core 无账本，
 *     契约 §4）。已接受的副作用：两段独立 acquire 的 region 若地址恰好相邻，
 *     free list 上的空闲块可能跨 region 合并——合并在同一分配器内是安全的，
 *     只是记不到"哪段归谁"。
 *   - `kcomp_heap_free` 不校验指针来源（GlobalAlloc 契约：ptr 必须来自同一堆的
 *     一次成功分配）。
 */
#ifndef KCOMP_KALLOC_H
#define KCOMP_KALLOC_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* backing 回调：与 `kcore_memory_acquire` 1:1。
 * 成功（0）时必须写 `*out_base` / `*out_len`，且 `*out_len >= min_len`；
 * 失败返回 `-Errno` 且不改 out。 */
typedef int (*kcomp_backing_fn)(size_t min_len, size_t min_align,
                                uintptr_t *out_base, size_t *out_len);

/* 把堆状态放进 `base` 处的 region（`len` 字节），返回句柄（正常 == base，
 * 因为 Core 的 region 至少页对齐）或 NULL（参数为空 / region 太小）。
 * 不分配：堆头 + 首个空闲块都写在这段 region 内。 */
void *kcomp_heap_place(void *base, size_t len, kcomp_backing_fn backing);

/* 分配 `size` 字节、`align` 对齐（align 必须是 2 的幂，0 非法）。
 * 失败（OOM / 溢出 / 非法 align / size == 0）返回 NULL。 */
void *kcomp_heap_alloc(void *heap, size_t size, size_t align);

/* 归还一次成功分配（NULL 安全，无动作）。 */
void kcomp_heap_free(void *heap, void *ptr);

/* 调整大小：成功返回可用指针（可能原地）；失败返回 NULL，旧块原样保留。
 * `ptr == NULL` 时等价于 alloc；`size == 0` 视为失败（no-op）。 */
void *kcomp_heap_realloc(void *heap, void *ptr, size_t size, size_t align);

#ifdef __cplusplus
}
#endif

#endif /* KCOMP_KALLOC_H */
