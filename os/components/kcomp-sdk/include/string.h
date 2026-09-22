/* string.h —— freestanding C 组件的字符串 / 内存原语声明（**不是 libc**）。
 *
 * clang `-ffreestanding` 不提供 `<string.h>`，但 C 组件会 `#include <string.h>`
 * （FatFs 的 ff.c 就是）。本 shim 只声明 SDK C 运行时（c/kcomp_rt.c）真正实现的
 * 那一小撮原语——需要更多时再往上加，这是**刻意的摩擦**：别让它悄悄长成 libc。
 *
 * tools/build-kcomp-c.sh 已把本目录（kcomp-sdk/include）放进 `-I`，所以组件的
 * `#include <string.h>` 会解析到这里。只对裸机 C 组件生效，host 构建不受影响。
 */
#ifndef KCOMP_STRING_H
#define KCOMP_STRING_H

#include <stddef.h>

/* -- 内存原语（clang 会为结构体赋值 / 清零合成这些调用）-- */
void *memcpy(void *dest, const void *src, size_t n);
void *memmove(void *dest, const void *src, size_t n);
void *memset(void *dest, int value, size_t n);
int memcmp(const void *a, const void *b, size_t n);

/* -- 字符串原语（显式调用；目前只有 FatFs 实际引用到这两个）-- */
size_t strlen(const char *s);
char *strchr(const char *s, int c);

#endif /* KCOMP_STRING_H */
