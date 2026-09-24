/* inttypes.h —— freestanding C 组件的定宽整数格式宏（**不是 libc**）。
 *
 * clang `-ffreestanding` 下 `#include <inttypes.h>` 会命中 clang 自带头，而它做
 * `#include_next <inttypes.h>`——裸机没有"下一个"，于是编译失败。littlefs 的
 * lfs_util.h 无条件 include 它，因此需要本 shim。
 *
 * 只提供 `<stdint.h>` 与 PRI* 格式宏；64 位 / 指针宽度按 `__SIZEOF_LONG__` /
 * `__SIZEOF_POINTER__` 分档，兼容 rv32(ilp32) 与 rv64(lp64)。需要更多时再往上加，
 * 这是**刻意的摩擦**：别让它悄悄长成 libc。
 *
 * tools/build-kcomp-c.sh 已把本目录放进 `-I`，故组件的 `#include <inttypes.h>`
 * 解析到这里；只对裸机 C 组件生效，host 构建不受影响。
 */
#ifndef KCOMP_INTTYPES_H
#define KCOMP_INTTYPES_H

#include <stdint.h>

/* 8 / 16 位 */
#define PRId8 "d"
#define PRIi8 "i"
#define PRIo8 "o"
#define PRIu8 "u"
#define PRIx8 "x"
#define PRIX8 "X"
#define PRId16 "d"
#define PRIi16 "i"
#define PRIo16 "o"
#define PRIu16 "u"
#define PRIx16 "x"
#define PRIX16 "X"

/* 32 位（所有 RISC-V ABI 上 int32_t 都是 int） */
#define PRId32 "d"
#define PRIi32 "i"
#define PRIo32 "o"
#define PRIu32 "u"
#define PRIx32 "x"
#define PRIX32 "X"

/* 64 位：lp64 → long；ilp32 → long long */
#if __SIZEOF_LONG__ == 8
#define KCOMP_PRI64_PREFIX "l"
#else
#define KCOMP_PRI64_PREFIX "ll"
#endif
#define PRId64 KCOMP_PRI64_PREFIX "d"
#define PRIi64 KCOMP_PRI64_PREFIX "i"
#define PRIo64 KCOMP_PRI64_PREFIX "o"
#define PRIu64 KCOMP_PRI64_PREFIX "u"
#define PRIx64 KCOMP_PRI64_PREFIX "x"
#define PRIX64 KCOMP_PRI64_PREFIX "X"

/* 指针宽度 */
#if __SIZEOF_POINTER__ == 8
#define PRIdPTR "ld"
#define PRIiPTR "li"
#define PRIoPTR "lo"
#define PRIuPTR "lu"
#define PRIxPTR "lx"
#define PRIXPTR "lX"
#else
#define PRIdPTR "d"
#define PRIiPTR "i"
#define PRIoPTR "o"
#define PRIuPTR "u"
#define PRIxPTR "x"
#define PRIXPTR "X"
#endif

/* intmax_t = long long */
#define PRIdMAX "lld"
#define PRIiMAX "lli"
#define PRIoMAX "llo"
#define PRIuMAX "llu"
#define PRIxMAX "llx"
#define PRIXMAX "llX"

#endif /* KCOMP_INTTYPES_H */
