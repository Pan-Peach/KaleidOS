#ifndef KALEIDOS_FATFS_INTERNAL_H
#define KALEIDOS_FATFS_INTERNAL_H

#include "kcomp.h"
#include "generated/filesystem_wire.h"
#include "ff.h"

/* 同时在线的 open 文件上限（handle 单调增长、不复用，0 永久保留为无效值）。 */
#define FATFS_MAX_OPEN_FILES 8

/* 只读挂载期间节点驻留；含根节点。卸载清表，token 单调增长、不复用。 */
#define FATFS_MAX_NODES 64

/* provider 定义的端口 token（**Gate** 路径经 kcomp_service_dispatch 用它选中本
 * 契约；Direct 路径不使用它）。provider 私有——组合策略不需要知道。 */
#define FATFS_PORT 1

/* 业务日志：QEMU runner 用它做差分断言（业务路径有一条 `[fatfs] <op>`；Gate 入口
 * 每次调用另有一条 `[fatfs] gate dispatch method=N`——两者对照即可证明稳态调用
 * 没有走 Core call gate）。只接受字符串字面量（`sizeof` 求长度）。 */
#define FATFS_LOG_LINE(text) kcore_log_line((const uint8_t *)(text), sizeof(text) - 1)

struct fatfs_file_slot
{
    uint64_t handle;
    FIL file;
    uint32_t consumer;
    uint32_t consumer_task;
};

struct fatfs_node
{
    uint64_t id;
    uint64_t parent;
    char path[KCOMP_FILESYSTEM_PATH_MAX];
    uint32_t kind;
    uint64_t size;
};

struct fatfs_state
{
    FATFS filesystem;

    /* Core 在 create 里选定的块调用绑定（机制藏在绑定内部）。 */
    struct kcomp_block_binding block_binding;

    uint32_t busy;
    uint64_t last_handle;
    int mounted;
    int alive;

    struct fatfs_file_slot files[FATFS_MAX_OPEN_FILES];
    struct fatfs_node nodes[FATFS_MAX_NODES];
    uint64_t last_node;
    uint32_t control;
    uint32_t ipc_only;
};

/* 不等待：同 CPU 的重入不能自旋；竞争返回 EBUSY，调用方决定重试。 */
static inline int fatfs_enter(struct fatfs_state *state)
{
    return state != NULL && !__atomic_exchange_n(&state->busy, 1, __ATOMIC_ACQUIRE);
}

static inline void fatfs_leave(struct fatfs_state *state)
{
    __atomic_store_n(&state->busy, 0, __ATOMIC_RELEASE);
}

/* 业务后端：Direct 的 `#[repr(C)]` function table 与 Gate 的扁平 method switch
 * （fatfs_service.c）调用**同一份**实现；业务代码不感知部署。 */
int32_t fatfs_mount(void *ctx);
int32_t fatfs_unmount(void *ctx);
int32_t fatfs_open(void *ctx, const char *path, uint32_t flags, uint64_t *out_handle);
int32_t fatfs_close(void *ctx, uint64_t handle);
int32_t fatfs_read(void *ctx, uint64_t handle, uint8_t *buf, size_t len, size_t *out_read);
int32_t fatfs_root(void *ctx, uint64_t *out_node);
int32_t fatfs_lookup(void *ctx, uint64_t parent, const uint8_t *name,
                     size_t name_len, uint32_t encoding, uint64_t *out_node);
int32_t fatfs_node_info(void *ctx, uint64_t node, uint32_t *out_kind);
int32_t fatfs_open_node(void *ctx, uint64_t node, uint64_t *out_handle);
int32_t fatfs_read_at(void *ctx, uint64_t handle, uint64_t offset,
                       uint8_t *buf, size_t len, size_t *out_read);
int32_t fatfs_dispatch(struct fatfs_state *state, uint32_t method,
                        const struct kcomp_call_frame *frame);
void fatfs_server(void *arg);

#endif /* KALEIDOS_FATFS_INTERNAL_H */
