#ifndef KALEIDOS_LITTLEFS_INTERNAL_H
#define KALEIDOS_LITTLEFS_INTERNAL_H

#include "kcomp.h"
#include "lfs.h"

/* 同时在线的 open 文件上限（handle 单调增长、不复用，0 永久保留为无效值）。 */
#define LITTLEFS_MAX_OPEN_FILES 8

/* provider 定义的端口 token（**Gate** 路径经 kcomp_service_dispatch 用它选中本
 * 契约；Direct 路径不使用它）。provider 私有——组合策略不需要知道。 */
#define LITTLEFS_PORT 1

/* littlefs 几何参数（`LFS_NO_MALLOC`：所有缓冲区显式提供，见 state）。约束
 * （lfs_init）：cache_size 是 read/prog_size 的整数倍，block_size 是 cache_size
 * 的整数倍，block_size >= 128，block_cycles != 0。block_size 取 block.device 的
 * sector 大小，于是 1 个 littlefs block 恰好 1 个 sector。 */
#define LITTLEFS_READ_SIZE 16
#define LITTLEFS_PROG_SIZE 16
#define LITTLEFS_BLOCK_SIZE KCOMP_BLOCK_DEVICE_SECTOR
#define LITTLEFS_CACHE_SIZE 16
#define LITTLEFS_LOOKAHEAD_SIZE 16
#define LITTLEFS_BLOCK_CYCLES 500

/* 自检文件（mount 后写入已知内容再读回校验；消费者也可 open 它）。 */
#define LITTLEFS_SELFTEST_PATH "selftest.txt"

/* 业务日志：QEMU runner 用它做差分断言（业务路径有一条 `[littlefs] <op>`；Gate
 * 入口每次调用另有一条 `[littlefs] gate dispatch method=N`——两者对照即可证明稳态
 * 调用没有走 Core call gate）。只接受字符串字面量（`sizeof` 求长度）。 */
#define LITTLEFS_LOG_LINE(text) kcore_log_line((const uint8_t *)(text), sizeof(text) - 1)

struct littlefs_file_slot {
    uint64_t handle;
    lfs_file_t file;
    /* `LFS_NO_MALLOC`：per-file cache buffer 必须显式提供，且 config 在文件打开
     * 期间保持存活（lfs_file_opencfg 的契约）——两者都放在本槽位里。 */
    struct lfs_file_config config;
    uint8_t buffer[LITTLEFS_CACHE_SIZE];
};

struct littlefs_state {
    lfs_t filesystem;
    struct lfs_config config;

    /* Core 在 create 里选定的块调用绑定（机制藏在绑定内部）。 */
    struct kcomp_block_binding block_binding;

    /* littlefs 的静态缓冲区（LFS_NO_MALLOC）：mount 的 read/prog/lookahead cache。 */
    uint8_t read_buffer[LITTLEFS_CACHE_SIZE];
    uint8_t prog_buffer[LITTLEFS_CACHE_SIZE];
    uint8_t lookahead_buffer[LITTLEFS_LOOKAHEAD_SIZE];

    /* block.device 只接受 512 字节整数倍的访问，而 littlefs 以 16 字节粒度访问：
     * 非整 sector 的 read/prog 经这个 bounce buffer 转一手。 */
    uint8_t sector[KCOMP_BLOCK_DEVICE_SECTOR];

    uint32_t busy;
    uint64_t last_handle;
    int mounted;
    int alive;

    struct littlefs_file_slot files[LITTLEFS_MAX_OPEN_FILES];

    /* 自检的专用文件槽（不占用公开 handle 槽位）。 */
    lfs_file_t selftest_file;
    struct lfs_file_config selftest_config;
    uint8_t selftest_buffer[LITTLEFS_CACHE_SIZE];
};

/* 不等待：同 CPU 的重入不能自旋；竞争返回 EBUSY，调用方决定重试。 */
static inline int littlefs_enter(struct littlefs_state *state)
{
    return state != NULL && !__atomic_exchange_n(&state->busy, 1, __ATOMIC_ACQUIRE);
}

static inline void littlefs_leave(struct littlefs_state *state)
{
    __atomic_store_n(&state->busy, 0, __ATOMIC_RELEASE);
}

/* 业务后端：Direct 的 `#[repr(C)]` function table 与 Gate 的扁平 method switch
 * （littlefs_service.c）调用**同一份**实现；业务代码不感知部署。 */
int32_t littlefs_mount(void *ctx);
int32_t littlefs_unmount(void *ctx);
int32_t littlefs_open(void *ctx, const char *path, uint32_t flags, uint64_t *out_handle);
int32_t littlefs_close(void *ctx, uint64_t handle);
int32_t littlefs_read(void *ctx, uint64_t handle, uint8_t *buf, size_t len, size_t *out_read);

#endif /* KALEIDOS_LITTLEFS_INTERNAL_H */
