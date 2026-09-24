#ifndef KALEIDOS_LITTLEFS_ADAPTER_H
#define KALEIDOS_LITTLEFS_ADAPTER_H

#include "littlefs_internal.h"
#include "lfs.h"

/* littlefs 的宿主回调（`lfs_config.read/prog/erase/sync`）都在 lfs_adapter.c 里
 * 翻译成 `kcomp_block_*` 的统一调用；`config.context` = 本实例 state，因此多实例
 * 互不干扰（没有全局磁盘胶水）。上游 lfs.c 对 KaleidOS 一无所知。 */

/* 填充 state->config（回调 + 几何参数 + 显式缓冲区）。create 时调用一次；
 * block_count 留 0，mount 前由 littlefs_adapter_capacity 按设备容量填。 */
void littlefs_adapter_init(struct littlefs_state *state);

/* 查询 block.device 容量（单位：512 字节 sector）到 `*out_sectors`。只读、不改
 * state；mount 前在 task 上下文调用（块调用契约禁止 trap / 中断上下文）。 */
int32_t littlefs_adapter_capacity(struct littlefs_state *state, uint64_t *out_sectors);

#endif /* KALEIDOS_LITTLEFS_ADAPTER_H */
