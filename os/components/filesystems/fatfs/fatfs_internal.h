#ifndef KALEIDOS_FATFS_INTERNAL_H
#define KALEIDOS_FATFS_INTERNAL_H

#include "kcomp.h"
#include "ff.h"

/* 同时在线的 open 文件上限（handle = slot + 1，0 永久保留为无效值）。 */
#define FATFS_MAX_OPEN_FILES 8

/* provider 定义的端口 token（**Gate** 路径经 kcomp_service_dispatch 用它选中本
 * 契约；Direct 路径不使用它）。provider 私有——组合策略不需要知道。 */
#define FATFS_PORT 1

/* 业务日志：QEMU runner 用它做差分断言（业务路径有一条 `[fatfs] <op>`；Gate 入口
 * 每次调用另有一条 `[fatfs] gate dispatch method=N`——两者对照即可证明稳态调用
 * 没有走 Core call gate）。只接受字符串字面量（`sizeof` 求长度）。 */
#define FATFS_LOG_LINE(text) kcore_log_line((const uint8_t *)(text), sizeof(text) - 1)

struct fatfs_file_slot {
    int used;
    FIL file;
};

struct fatfs_state {
    FATFS filesystem;

    /* Core 在 create 里选定的块调用绑定（机制藏在绑定内部）。 */
    struct kcomp_block_binding block_binding;

    int mounted;
    int alive;

    struct fatfs_file_slot files[FATFS_MAX_OPEN_FILES];
};

/* 业务后端：Direct 的 `#[repr(C)]` function table 与 Gate 的扁平 method switch
 * （fatfs_service.c）调用**同一份**实现；业务代码不感知部署。 */
int32_t fatfs_mount(void *ctx);
int32_t fatfs_unmount(void *ctx);
int32_t fatfs_open(void *ctx, const char *path, uint32_t flags, uint64_t *out_handle);
int32_t fatfs_close(void *ctx, uint64_t handle);
int32_t fatfs_read(void *ctx, uint64_t handle, uint8_t *buf, size_t len, size_t *out_read);

#endif /* KALEIDOS_FATFS_INTERNAL_H */
