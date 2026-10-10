/* Typed IPC BlockDevice consumer facade; 512-byte splitting stays inside the SDK. */
#ifndef KCOMP_BLOCK_H
#define KCOMP_BLOCK_H

#include <stddef.h>
#include <stdint.h>

/* transport is Core IPC status; method is provider 0/-errno and valid only
 * when transport == 0. A later sector failure preserves prior completed I/O. */
#include "kcomp_call.h"

/* 不透明调用绑定（内部表示）。组件只声明它、把它原样传回 `kcomp_block_*`——
 * 字段是实现细节，不要在组件代码里解释。 */
struct kcomp_block_binding {
    uint64_t opaque[4];
};

/* Exact IPC endpoint validation; composer grants send rights separately. */
int32_t kcomp_block_bind(uint64_t endpoint, uint64_t contract, uint64_t abi,
                         struct kcomp_block_binding *out_binding);

/* Read a nonempty multiple of 512 bytes into a valid business buffer.
 * The SDK validates last-LBA overflow before submitting any sector. */
struct kcomp_call_result kcomp_block_read(const struct kcomp_block_binding *binding,
                                          uint64_t lba, void *output, size_t output_len);

/* Write a nonempty multiple of 512 bytes from a valid business buffer.
 * Successful earlier sectors are not rolled back if a later sector fails. */
struct kcomp_call_result kcomp_block_write(const struct kcomp_block_binding *binding,
                                           uint64_t lba, const void *input, size_t input_len);

/* 读设备容量（单位：512 字节 sector）到 `*out_sectors`。
 * 成功（transport == 0 且 method == 0）时 `*out_sectors` 有效。 */
struct kcomp_call_result kcomp_block_capacity(const struct kcomp_block_binding *binding,
                                              uint64_t *out_sectors);

#endif /* KCOMP_BLOCK_H */
