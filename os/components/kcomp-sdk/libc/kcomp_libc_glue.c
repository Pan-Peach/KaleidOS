/* kcomp_libc_glue.c —— 可选：把一个完整 libc（picolibc）接到 KaleidOS 的 host-glue。
 *
 * 这是 porting.md §3 说的"SDK 可选 host-glue"：picolibc 不是 drop-in，它要宿主提供
 * 几个底层原语。本文件把每个原语接到**已存在**的 Core 机制上（不新增机制）：
 *
 *   _write  → kcore_log_line（core 格式化 + arch Console backend 传输）
 *   _sbrk   → 组件私有的静态 arena（malloc 的 backing）
 *   _exit   → 尽力结束当前 task（noreturn）
 *
 * **opt-in，不随默认构建链接。** tools/build-kcomp-c.sh 自动编入 `kcomp-sdk/c/` 下的全部 C 源；
 * 本文件刻意放在 `libc/`（不在 `c/` 下），因此只有**显式**把它加进自己的
 * `kcomp-c-src.txt` 的组件才会链接它——也就是那些同时链接 picolibc `libc.a` 的组件。
 * 这样默认的 freestanding C 组件（FatFs / littlefs / kcomp_c_smoke）不受影响。
 *
 * 组件侧 opt-in 形状（提案，尚未有组件采用）：
 *   kcomp-c-src.txt 追加：os/components/kcomp-sdk/libc/kcomp_libc_glue.c
 *   CFLAGS 追加：-I<third_party/picolibc 构建产物的 include>
 *   链接：picolibc 的 libc.a + clang 内建 `-lclang_rt.builtins`
 *
 * 构建 recipe（自定义 cross file）见 docs/architecture/porting.md §6.4。
 *
 * 注意：`__ashlti3` / `__lshrti3`（picolibc 的 snprintf 等会引用）**不在这里实现**——
 * 它们应由 clang 的 compiler-rt builtins 提供（链接 `libclang_rt.builtins`）。手写
 * 128 位 ABI helper 既易错又与编译器版本耦合，属于构建集成（见 §6.4）。
 */
#include "kcomp.h"
#include <stddef.h>
#include <stdint.h>

/* --- _write：picolibc tinystdio 的输出落到 Core console（arch Console backend）。 --- */
long _write(int fd, const void *buf, size_t count)
{
    /* tinystdio 只用 fd 1 / 2；KaleidOS 阶段一无 fd 表，全部落 console。 */
    (void)fd;

    if (buf == NULL && count != 0)
    {
        return -1;
    }

    if (count != 0)
    {
        kcore_log_line((const uint8_t *)buf, count);
    }

    return (long)count;
}

/* --- _sbrk：malloc 的 backing。组件私有静态 arena（每个 .kcomp 自带一份，不是
 * shared runtime）；失败返回 (void *)-1（picolibc 的 malloc 据此报 ENOMEM）。 --- */
#ifndef KCOMP_LIBC_HEAP_SIZE
#define KCOMP_LIBC_HEAP_SIZE (64u * 1024u)
#endif

static unsigned char kcomp_libc_heap[KCOMP_LIBC_HEAP_SIZE];
static size_t kcomp_libc_break;

void *_sbrk(intptr_t incr)
{
    /* 只增长、不收缩；8 字节对齐满足 malloc 的对齐需求。picolibc 明确支持 sbrk
     * 返回不连续内存，这里给连续 arena 是最简形态。 */
    size_t need = (size_t)incr;
    size_t aligned = (kcomp_libc_break + 7u) & ~(size_t)7u;

    if (incr < 0 || need > KCOMP_LIBC_HEAP_SIZE - aligned)
    {
        return (void *)-1;
    }

    void *previous = &kcomp_libc_heap[aligned];
    kcomp_libc_break = aligned + need;
    return previous;
}

/* --- _exit：noreturn。尽力结束当前 task；若不在 task 上下文则停在此处（panic
 * containment 会兜住）。picolibc 的 exit / abort / raise 都落到它。 --- */
void _exit(int status)
{
    (void)status;
    (void)kcore_task_exit();

    for (;;)
    {
    }
}
