/* kcomp_block.h —— block.device 的 endpoint 调用 C 前端（consumer 侧）。
 *
 * 必须经 `kcomp.h` include（`kcore_endpoint_bind` / `KCOMP_BLOCK_*` 的声明来自
 * `generated/kcomp_abi.h`）；不建议单独 include 本文件。
 *
 * 与 SDK-Rust 的 typed 前端（`src/block/client.rs`）使用**同一条线格式**与同一个
 * 绑定形状：消费者只声明一个**不透明** `struct kcomp_block_binding`，调
 * `kcomp_block_bind` 取得它，再用统一的 `kcomp_block_read` / `kcomp_block_capacity`
 * 调用。机制（Direct / Gate）由 Core 在 bind 时按 (caller, provider) 执行域选定，
 * 藏在绑定内部——消费者看不到、也不得解释它（**不暴露裸 function table**）。
 *
 * 线格式（`abi/block.toml` 单源）：
 *   read(1)：args = 8 字节 LE `u64` lba；output 非零且 512 的整数倍
 *            （传输长度就是 `output_len`，没有单独编码的长度）。
 *
 * 实现（不透明绑定的内部表示）在 `kcomp-sdk/c/kcomp_block.c`，随每个 C 组件私有
 * 携带；本头文件只暴露声明。
 */
#ifndef KCOMP_BLOCK_H
#define KCOMP_BLOCK_H

#include <stddef.h>
#include <stdint.h>

/* Core 传输状态 ≠ provider 方法状态（共享类型，见 kcomp_call.h）：provider 自己的
 * `0 / -errno` 在 method 里，只有 transport == 0 时有意义（与
 * `kcore_endpoint_call` 的 ABI 契约一致）。Direct 绑定没有传输层：transport 恒 0，
 * method 就是 function table 的返回。 */
#include "kcomp_call.h"

/* 不透明调用绑定（内部表示）。组件只声明它、把它原样传回 `kcomp_block_*`——
 * 字段是实现细节（Core 选定的机制藏在里面），不要在组件代码里解释。 */
struct kcomp_block_binding {
    uint64_t opaque[4];
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

/* bind：调 Core（exact contract + abi + 存活校验 → 一次性选定机制）。
 *
 * `endpoint` 是组合策略经 create config 交给本组件的 opaque EndpointId（消费者
 * **不**做全局名字发现）；`contract` / `abi` 是本契约的编译期常量
 * （`KCOMP_BLOCK_DEVICE_CONTRACT` / `KCOMP_BLOCK_DEVICE_ABI`）。
 * 成功 = 0（绑定可立即用于 read / capacity）；失败 = -errno。 */
int32_t kcomp_block_bind(uint64_t endpoint, uint64_t contract, uint64_t abi,
                         struct kcomp_block_binding *out_binding);

/* 从绑定读 `output_len` 字节到 `output`（长度 / 对齐由调用方保证，与
 * `kcore_endpoint_call` 的契约一致：长度非零时指针不得为空）。
 * 成功（transport == 0）时 provider 的 `0 / -errno` 在 `result.method`。 */
struct kcomp_call_result kcomp_block_read(const struct kcomp_block_binding *binding,
                                          uint64_t lba, void *output, size_t output_len);

/* 读设备容量（单位：512 字节 sector）到 `*out_sectors`。
 * 成功（transport == 0 且 method == 0）时 `*out_sectors` 有效。 */
struct kcomp_call_result kcomp_block_capacity(const struct kcomp_block_binding *binding,
                                              uint64_t *out_sectors);

#endif /* KCOMP_BLOCK_H */
