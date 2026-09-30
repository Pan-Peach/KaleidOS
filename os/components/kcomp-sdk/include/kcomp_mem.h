/* kcomp_mem.h —— 组件面向的 **raw backing 便利分配器**（freestanding，无 libc）。
 *
 * 契约：docs/architecture/memory-and-heap.md §2。薄包装 Core 的域视图 ABI
 * `kcore_memory_acquire` / `kcore_memory_release`：取一段 backing，返回**本执行域
 * 访问窗口**（`struct kcore_memory_view`），释放凭同一个 view。
 *
 * **这不是堆**（KernelNative 的普通 malloc/free 走 `kcore_heap_alloc/dealloc`，
 * 共享 Core 堆；私有执行域的后端是 `kcomp_kalloc.h`）：这里是"向 Core 取一段
 * backing"的薄封装，也是 Isolated / Sandboxed 私有分配器的 backing 来源。
 *
 * 语义（与 Core 契约逐字一致，本层不添加策略）：
 *   - `kcomp_mem_alloc`：`min_len > 0`、`min_align` 为非零 2 的幂；成功 = 0 且写
 *     `*out`（`out->len >= min_len`，首次交付**零初始化**）；失败 = `-errno` 且
 *     不改 out。访问窗口 = `out->base` / `out->len`。
 *   - `kcomp_mem_free`：把**同一个** view 原样交回；成功 = 0，失败 = `-errno`。
 *   - **无账本**：Core 不记 owner / 不发 id——view 自身就是身份。
 *
 * 实现随每个 C 组件私有携带（tools/build-kcomp-c.sh 编译 kcomp-sdk/c/ 下全部 .c）。
 */
#ifndef KCOMP_MEM_H
#define KCOMP_MEM_H

#include <stdint.h>

#include "generated/kcomp_abi.h"

#ifdef __cplusplus
extern "C" {
#endif

/* 取一段 backing，返回本执行域访问窗口。成功 = 0 / 失败 = -errno（不改 out）。 */
int32_t kcomp_mem_alloc(struct kcore_memory_view *out, uint64_t min_len, uint64_t min_align);

/* 交回一个 kcomp_mem_alloc 交付的 view（原样交回）。成功 = 0 / 失败 = -errno。 */
int32_t kcomp_mem_free(const struct kcore_memory_view *view);

#ifdef __cplusplus
}
#endif

#endif /* KCOMP_MEM_H */
