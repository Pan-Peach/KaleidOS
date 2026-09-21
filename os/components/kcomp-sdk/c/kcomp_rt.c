/* kcomp_rt.c —— 组件 SDK 的 C 运行时（CRT）：C 组件共用的最底层原语。
 *
 * C 组件（不含 Rust 的 compiler_builtins）在 freestanding 环境里需要自己提供
 * `memcpy` / `memset` / `memmove` / `memcmp`：clang 会对结构体赋值 / 清零合成这些
 * 调用。这里给出 **weak** 定义，由 tools/build-kcomp-c.sh 随每个 C 组件编入；
 * weak 保证即使某天与别的 weak 提供者（如 Rust compiler_builtins）同处一个镜像，
 * 也不会重复定义。
 *
 * C support ≠ libc support：只提供这四个最底层原语，不引入任何 libc。
 *
 * 注意：组件对 Core 的调用（`kcore_*`）由 `include/kcomp.h` 声明、由 Core 导出
 * 白名单在加载时解析，**不在这里包一层**——`kcore_*` 是唯一的名字。
 */
#include <stddef.h>

__attribute__((weak)) void *memcpy(void *dest, const void *src, size_t n) {
    unsigned char *d = (unsigned char *)dest;
    const unsigned char *s = (const unsigned char *)src;
    for (size_t i = 0; i < n; i++) {
        d[i] = s[i];
    }
    return dest;
}

__attribute__((weak)) void *memset(void *dest, int value, size_t n) {
    unsigned char *d = (unsigned char *)dest;
    for (size_t i = 0; i < n; i++) {
        d[i] = (unsigned char)value;
    }
    return dest;
}

__attribute__((weak)) void *memmove(void *dest, const void *src, size_t n) {
    unsigned char *d = (unsigned char *)dest;
    const unsigned char *s = (const unsigned char *)src;
    if (d == s || n == 0) {
        return dest;
    }
    if (d < s) {
        for (size_t i = 0; i < n; i++) {
            d[i] = s[i];
        }
    } else {
        for (size_t i = n; i > 0; i--) {
            d[i - 1] = s[i - 1];
        }
    }
    return dest;
}

__attribute__((weak)) int memcmp(const void *a, const void *b, size_t n) {
    const unsigned char *x = (const unsigned char *)a;
    const unsigned char *y = (const unsigned char *)b;
    for (size_t i = 0; i < n; i++) {
        if (x[i] != y[i]) {
            return x[i] < y[i] ? -1 : 1;
        }
    }
    return 0;
}
