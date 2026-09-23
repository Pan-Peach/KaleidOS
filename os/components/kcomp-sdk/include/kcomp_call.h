/* kcomp_call.h —— endpoint 调用包装的公共结果类型。
 *
 * `kcomp_block.h` / `kcomp_filesystem.h` 共享同一份 `struct kcomp_call_result`：
 * 把 Core 的**传输状态**与 provider 的**方法状态**分开（两者在
 * `kcore_endpoint_call` 的 ABI 上就是分离的：provider 自己的 `0 / -errno` 只在
 * transport == 0 时有意义）。
 *
 * 必须经 `kcomp.h` include（常量 / 生成声明的本体在 `generated/kcomp_abi.h`）；
 * 本文件只放这一个手写类型，避免每个契约包装各定义一份、定义漂移。
 */
#ifndef KCOMP_CALL_H
#define KCOMP_CALL_H

#include <stdint.h>

struct kcomp_call_result {
    /* Core 的传输状态：0 = provider 已被调用；<0 = -errno（method 无意义）。 */
    int32_t transport;
    /* provider 自己的 0 / -errno；仅当 transport == 0 时有意义。 */
    int32_t method;
};

#endif /* KCOMP_CALL_H */
