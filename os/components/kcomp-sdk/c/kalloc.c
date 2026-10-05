/* kalloc.c —— 私有执行域运行时堆：freestanding C 实现（单一真相）。
 *
 * Isolated 的私有分配器后端（Sandboxed 执行后端后置）：
 * KernelNative 普通 malloc/free 走 Core 的 `kcore_heap_alloc` / `kcore_heap_dealloc`
 * （同特权同 AS 共享 Core 堆）；私有执行域才在自己的可写 image 里用本分配器。
 * 契约：docs/architecture/memory-and-heap.md §6。
 *
 *   - 侵入式、按地址升序的 **coalescing free list**；堆头 / 块头全部在 region
 *     内部，增长（backing 回调）不产生任何辅助分配。
 *   - 分配器自己**绝不 panic、绝不分配**：失败一律返回 NULL。
 *   - Rust 侧 facade 在 kcomp-sdk/src/heap.rs；C 组件直接
 *     `#include "kcomp_kalloc.h"`。
 *   - 只导出 `kcomp_heap_*`；**不**定义 malloc / free / calloc / realloc
 *     （避免与 kcomp_rt.c / picolibc 冲突）。
 *
 * 布局（全部在 region 内部）：
 *
 *   堆头 kcomp_heap 放在 kcomp_heap_place 给的 region base；其余空间是一个
 *   初始空闲块。每个块：
 *
 *     free 块：  [ kcomp_block | 未使用空间 .......... ]
 *     alloc 块： [ 对齐填充 pad | kcomp_block | payload .......... ]
 *
 *   - `kcomp_block.size` = 从**块首**到块尾的总字节数（含 pad），恒为
 *     KCOMP_ALIGN_MIN 的倍数。
 *   - alloc 块的块头紧贴 payload 之前；`pad` = 块头距块首的字节数
 *     （`kcomp_heap_free` 用它从 payload 找回块首）。pad < MIN_BLOCK 时直接
 *     吸收进分配块（不切分），因此不丢字节。
 *   - 对齐：payload 地址按请求 align 上取整（align < 指针宽时按指针宽）；
 *     块首始终 KCOMP_ALIGN_MIN 对齐，所以相邻块总能被 free list 合并。
 *
 * v1 边界（明确记录）：
 *   - **不支持 IRQ 上下文分配**：v1 无锁；将来若加单 CPU 自旋锁，中断里分配
 *     会自死锁。
 *   - **没有 region list**：整段 release 不在 v1 范围内。已接受的副作用：
 *     两段独立 acquire 的 region 若地址恰好相邻，空闲块可能跨 region 合并
 *     ——合并在同一分配器内安全，只是记不到"哪段归谁"。
 */

#include "kcomp_kalloc.h"

/* 自然对齐下限（指针宽）：payload / 块头都保持在这个粒度。 */
#define KCOMP_ALIGN_MIN ((size_t)sizeof(void *))

/* 几何式增长的起步与上限（请求容量，不是物理占用承诺）。 */
#define GROW_MIN ((size_t)4096)
#define GROW_MAX ((size_t)(1024u * 1024u))

typedef struct kcomp_block {
    struct kcomp_block *next; /* free list 链（仅 free 块有效） */
    size_t size;              /* 块首 → 块尾总字节数（含 pad） */
    size_t pad;               /* alloc 块：块头距块首的字节数；free 块恒 0 */
} kcomp_block;

typedef struct kcomp_heap {
    kcomp_backing_fn backing;
    kcomp_block *free_list; /* 按地址升序 */
    size_t next_grow;       /* 下一次向 backing 请求的几何式容量 */
} kcomp_heap;

#define HDR ((size_t)sizeof(kcomp_block))
#define HEAP_HDR ((size_t)sizeof(kcomp_heap))
/* 一个空闲块至少能装下：块头 + 指针宽的 payload（最小分配）。 */
#define MIN_BLOCK (HDR + KCOMP_ALIGN_MIN)

static int is_pow2(size_t v) {
    return v != 0 && (v & (v - 1)) == 0;
}

static uintptr_t align_up_addr(uintptr_t v, size_t align) {
    return (v + ((uintptr_t)align - 1)) & ~((uintptr_t)align - 1);
}

static void copy_bytes(void *dst, const void *src, size_t n) {
    unsigned char *d = (unsigned char *)dst;
    const unsigned char *s = (const unsigned char *)src;
    for (size_t i = 0; i < n; i++) {
        d[i] = s[i];
    }
}

/* 按地址升序插入 free list，并合并相邻空闲块（前向 + 后向）。
 * 所有块都在同一分配器管理的 region 内；比较统一走 uintptr_t。 */
static void insert_free(kcomp_heap *heap, kcomp_block *block) {
    kcomp_block *prev = (kcomp_block *)0;
    kcomp_block *cur = heap->free_list;
    while (cur != (kcomp_block *)0 && (uintptr_t)cur < (uintptr_t)block) {
        prev = cur;
        cur = cur->next;
    }
    if (prev != (kcomp_block *)0 &&
        (unsigned char *)prev + prev->size == (unsigned char *)block) {
        prev->size += block->size;
        block = prev;
    } else {
        block->next = cur;
        if (prev == (kcomp_block *)0) {
            heap->free_list = block;
        } else {
            prev->next = block;
        }
    }
    if (cur != (kcomp_block *)0 &&
        (unsigned char *)block + block->size == (unsigned char *)cur) {
        block->size += cur->size;
        block->next = cur->next;
    }
}

/* 没有可用块时向 backing 请求一段新 region，插入 free list。
 * 返回 0 = 没有新内存可用（backing 失败 / region 太小）。 */
static int grow(kcomp_heap *heap, size_t need) {
    size_t want = heap->next_grow;
    if (want < need) {
        want = need;
    }
    uintptr_t base = 0;
    size_t len = 0;
    if (heap->backing(want, KCOMP_ALIGN_MIN, &base, &len) != 0 || base == 0 ||
        len < need) {
        return 0;
    }
    uintptr_t start = align_up_addr(base, KCOMP_ALIGN_MIN);
    size_t skipped = (size_t)(start - base);
    if (len <= skipped) {
        return 0;
    }
    size_t avail = (len - skipped) & ~(KCOMP_ALIGN_MIN - 1);
    if (avail < MIN_BLOCK) {
        return 0;
    }
    kcomp_block *block = (kcomp_block *)start;
    block->size = avail;
    block->pad = 0;
    insert_free(heap, block);

    /* 几何式请求容量；封顶 GROW_MAX（溢出安全：先比较再翻倍）。 */
    if (len >= GROW_MAX / 2) {
        heap->next_grow = GROW_MAX;
    } else {
        heap->next_grow = len * 2;
    }
    if (heap->next_grow < GROW_MIN) {
        heap->next_grow = GROW_MIN;
    }
    return 1;
}

void *kcomp_heap_place(void *base, size_t len, kcomp_backing_fn backing) {
    if (base == (void *)0 || backing == (kcomp_backing_fn)0) {
        return (void *)0;
    }
    uintptr_t start = align_up_addr((uintptr_t)base, KCOMP_ALIGN_MIN);
    size_t skipped = (size_t)(start - (uintptr_t)base);
    if (len <= skipped || len - skipped < HEAP_HDR + MIN_BLOCK) {
        return (void *)0;
    }
    size_t avail = (len - skipped - HEAP_HDR) & ~(KCOMP_ALIGN_MIN - 1);
    if (avail < MIN_BLOCK) {
        return (void *)0;
    }
    kcomp_heap *heap = (kcomp_heap *)start;
    heap->backing = backing;
    heap->free_list = (kcomp_block *)0;
    heap->next_grow = GROW_MIN;

    kcomp_block *block = (kcomp_block *)(start + HEAP_HDR);
    block->size = avail;
    block->pad = 0;
    insert_free(heap, block);
    return (void *)start;
}

void *kcomp_heap_alloc(void *heap_ptr, size_t size, size_t align) {
    kcomp_heap *heap = (kcomp_heap *)heap_ptr;
    if (heap == (kcomp_heap *)0 || size == 0 || !is_pow2(align)) {
        return (void *)0;
    }
    if (align < KCOMP_ALIGN_MIN) {
        align = KCOMP_ALIGN_MIN;
    }
    if (size > SIZE_MAX - (align - 1)) {
        return (void *)0;
    }
    size_t rounded = (size + (align - 1)) & ~(align - 1);
    if (rounded > SIZE_MAX - HDR) {
        return (void *)0;
    }
    size_t need = rounded + HDR;

    for (int attempt = 0; attempt < 2; attempt++) {
        kcomp_block *prev = (kcomp_block *)0;
        kcomp_block *block = heap->free_list;
        while (block != (kcomp_block *)0) {
            uintptr_t payload = align_up_addr((uintptr_t)block + HDR, align);
            size_t pad = (size_t)(payload - HDR - (uintptr_t)block);
            if (pad <= SIZE_MAX - need && block->size >= pad + need) {
                break;
            }
            prev = block;
            block = block->next;
        }
        if (block != (kcomp_block *)0) {
            /* 从 free list 摘下（split 出来的前后缀稍后各自插回）。 */
            if (prev == (kcomp_block *)0) {
                heap->free_list = block->next;
            } else {
                prev->next = block->next;
            }
            uintptr_t payload = align_up_addr((uintptr_t)block + HDR, align);
            size_t pad = (size_t)(payload - HDR - (uintptr_t)block);
            size_t total = pad + need;
            size_t rest = block->size - total;
            size_t alloc_size = need;
            kcomp_block *header = (kcomp_block *)(payload - HDR);

            if (pad >= MIN_BLOCK) {
                /* 前缀够大：独立成 free 块，分配块块首 = header（pad = 0）。 */
                kcomp_block *prefix = block;
                prefix->size = pad;
                prefix->pad = 0;
                insert_free(heap, prefix);
                header->pad = 0;
            } else {
                /* 前缀太小：吸收进分配块（块首仍是 block，free 按 pad 找回）。 */
                header->pad = pad;
            }
            if (rest >= MIN_BLOCK) {
                kcomp_block *remainder =
                    (kcomp_block *)((unsigned char *)block + total);
                remainder->size = rest;
                remainder->pad = 0;
                insert_free(heap, remainder);
            } else {
                /* 尾料太小：吸收进分配块，不制造不可用的碎片。 */
                alloc_size += rest;
            }
            header->size = alloc_size;
            header->next = (kcomp_block *)0;
            return (void *)payload;
        }
        /* 第一次没找到：向 backing 要一段新 region 再试一次。 */
        if (attempt == 0 && !grow(heap, need)) {
            return (void *)0;
        }
    }
    return (void *)0;
}

void kcomp_heap_free(void *heap_ptr, void *ptr) {
    kcomp_heap *heap = (kcomp_heap *)heap_ptr;
    if (heap == (kcomp_heap *)0 || ptr == (void *)0) {
        return;
    }
    kcomp_block *header = (kcomp_block *)((unsigned char *)ptr - HDR);
    size_t size = header->size;
    uintptr_t start = (uintptr_t)header - header->pad;
    kcomp_block *block = (kcomp_block *)start;
    block->size = size;
    block->pad = 0;
    insert_free(heap, block);
}

void *kcomp_heap_realloc(void *heap_ptr, void *ptr, size_t size, size_t align) {
    kcomp_heap *heap = (kcomp_heap *)heap_ptr;
    if (heap == (kcomp_heap *)0) {
        return (void *)0;
    }
    if (ptr == (void *)0) {
        return kcomp_heap_alloc(heap_ptr, size, align);
    }
    if (size == 0 || !is_pow2(align)) {
        return (void *)0; /* 失败：旧块原样保留 */
    }
    if (align < KCOMP_ALIGN_MIN) {
        align = KCOMP_ALIGN_MIN;
    }
    if (size > SIZE_MAX - (align - 1)) {
        return (void *)0;
    }
    kcomp_block *header = (kcomp_block *)((unsigned char *)ptr - HDR);
    uintptr_t start = (uintptr_t)header - header->pad;
    size_t capacity = (size_t)(start + header->size - (uintptr_t)ptr);

    if (capacity >= size && ((uintptr_t)ptr & ((uintptr_t)align - 1)) == 0) {
        return ptr; /* 原地满足（含缩小；不切分，保持简单） */
    }
    void *fresh = kcomp_heap_alloc(heap_ptr, size, align);
    if (fresh == (void *)0) {
        return (void *)0; /* 旧块原样保留 */
    }
    size_t copy = capacity < size ? capacity : size;
    copy_bytes(fresh, ptr, copy);
    kcomp_heap_free(heap_ptr, ptr);
    return fresh;
}
