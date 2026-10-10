#ifndef KALEIDOS_FATFS_INTERNAL_H
#define KALEIDOS_FATFS_INTERNAL_H

#include "kcomp.h"
#include "generated/filesystem_wire.h"
#include "ff.h"

/* 同时在线的 open 文件上限（handle 单调增长、不复用，0 永久保留为无效值）。 */
#define FATFS_MAX_OPEN_FILES 8

/* 只读挂载期间节点驻留；含根节点。卸载清表，token 单调增长、不复用。 */
#define FATFS_MAX_NODES 64

/* Business logs accept literal strings. */
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

    /* Exact IPC Block endpoint; send rights come from composition. */
    struct kcomp_block_binding block_binding;

    uint32_t busy;
    uint64_t last_handle;
    int mounted;
    int alive;

    struct fatfs_file_slot files[FATFS_MAX_OPEN_FILES];
    struct fatfs_node nodes[FATFS_MAX_NODES];
    uint64_t last_node;
    uint32_t control;
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

/* Local business functions, called by the generated IPC handlers. */
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
void fatfs_server(void *arg);

#endif /* KALEIDOS_FATFS_INTERNAL_H */
