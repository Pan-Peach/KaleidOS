/* kcomp_block.h —— block.device 的 **endpoint 调用** C 包装（手写草案）。
 *
 * 必须经 `kcomp.h` include（`kcore_endpoint_call` / `KCOMP_BLOCK_*` 的声明来自
 * `generated/kcomp_abi.h`）；不建议单独 include 本文件。
 *
 * 与 SDK-Rust 的 typed 前端（`src/block/client.rs`）使用**同一条线格式**：
 *   read(1)：args = 8 字节 LE `u64` lba；output 非零且 512 的整数倍
 *            （传输长度就是 `output_len`，没有单独编码的长度）。
 * 方法号 / 长度 / sector 常量都是 `abi/block.toml` 的单源生成物。
 *
 * 边界（草案）：
 *   - 只覆盖 `read`（capacity / write 包装等 consumer 迁移时再补）；
 *   - `struct kcomp_call_result` 是 C 内部的返回值形状，**不是**跨语言布局约定：
 *     没有 C layout cast、没有对齐假设、没有嵌套指针、没有 Rust ABI；
 *   - 不解析回复语义（capacity 的 8 字节 LE 回复由调用方解码）。
 */
#ifndef KCOMP_BLOCK_H
#define KCOMP_BLOCK_H

#include <stddef.h>
#include <stdint.h>

/* Core 传输状态 ≠ provider 方法状态：provider 自己的 `0 / -errno` 在 method 里，
 * 只有 transport == 0 时有意义（与 `kcore_endpoint_call` 的 ABI 契约一致）。 */
struct kcomp_call_result {
    /* Core 的传输状态：0 = provider 已被调用；<0 = -errno（method 无意义）。 */
    int32_t transport;
    /* provider 自己的 0 / -errno；仅当 transport == 0 时有意义。 */
    int32_t method;
};

/* LE 编码：第 i 字节 = lba >> (8*i)（不依赖宿主字节序，不做布局 cast）。 */
#define KCOMP_BLOCK_LBA_BYTE(lba, i) \
    ((uint8_t)(((uint64_t)(lba) >> (8u * (uint32_t)(i))) & 0xFFu))

/* 编译期把 C 侧编码钉到与 Rust `block::dispatch::encode_lba` 相同的固定向量上
 * （见 SDK 测试 `block_wire_format_matches_the_c_wrapper_encoding`）：
 * 改任一侧的字节序 / 位序，这里编译失败。 */
_Static_assert(KCOMP_BLOCK_LBA_BYTE(0x0102030405060708ULL, 0) == 0x08u, "block wire format drift");
_Static_assert(KCOMP_BLOCK_LBA_BYTE(0x0102030405060708ULL, 1) == 0x07u, "block wire format drift");
_Static_assert(KCOMP_BLOCK_LBA_BYTE(0x0102030405060708ULL, 7) == 0x01u, "block wire format drift");
_Static_assert(KCOMP_BLOCK_LBA_LEN == 8u, "block wire format drift (lba len)");
_Static_assert(KCOMP_BLOCK_CAPACITY_LEN == 8u, "block wire format drift (capacity len)");

/* 从 endpoint 读 `output_len` 字节（`endpoint` = 组合期发现的 opaque EndpointId）。
 *
 * 成功（transport == 0）时 provider 的 `0 / -errno` 在 `result.method`；
 * `output` 的长度 / 对齐由调用方保证（与 `kcore_endpoint_call` 的契约一致：
 * 长度非零时指针不得为空）。 */
static inline struct kcomp_call_result kcomp_block_read(uint64_t endpoint, uint64_t lba,
                                                        void *output, size_t output_len) {
    uint8_t args[KCOMP_BLOCK_LBA_LEN];
    int32_t method_status = 0;
    struct kcomp_call_result result;

    for (size_t i = 0; i < KCOMP_BLOCK_LBA_LEN; i++) {
        args[i] = KCOMP_BLOCK_LBA_BYTE(lba, i);
    }
    result.transport = kcore_endpoint_call(endpoint, KCOMP_BLOCK_METHOD_READ, args, sizeof(args),
                                           NULL, 0, (uint8_t *)output, output_len, &method_status);
    result.method = method_status;
    return result;
}

#endif /* KCOMP_BLOCK_H */
