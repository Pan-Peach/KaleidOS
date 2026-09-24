/* kcomp_mem.c —— kcomp_mem.h 的实现：Core 域视图 ABI 的薄包装。
 *
 * 契约：docs/architecture/memory-and-heap.md §2。本文件**不添加策略**（size /
 * align 校验、零初始化、无账本语义都在 Core 侧），只把组件作者面固定下来，
 * 与 Rust 镜像 `src/mem.rs` 逐函数对应。
 */
#include "kcomp.h"

int32_t kcomp_mem_alloc(struct kcore_memory_view *out, uint64_t min_len, uint64_t min_align)
{
    return kcore_memory_acquire(min_len, min_align, out);
}

int32_t kcomp_mem_free(const struct kcore_memory_view *view)
{
    return kcore_memory_release(view);
}
