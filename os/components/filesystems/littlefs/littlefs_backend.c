/* littlefs_backend.c —— littlefs 的**业务后端**（只读 littlefs 文件系统语义）。
 *
 * Direct 的 `#[repr(C)]` function table（littlefs.c）与 Gate 的扁平 method switch
 * （littlefs_service.c）调用**同一份**实现；业务代码不感知部署。每个业务方法成功时
 * 打一行 `[littlefs] <op>`——QEMU runner 用它对照 Gate 入口日志做差分断言。
 *
 * 挂载策略（与 FatFs 同构）：`lfs_mount` 失败（未格式化）→ `lfs_format` → 再
 * `lfs_mount`；成功后跑一次自检（写已知内容、只读读回、逐字节校验），既证明
 * prog/erase/read 通路可用，也给消费者留下可读内容。
 */
#include "kcomp.h"
#include "lfs_adapter.h"
#include "littlefs_internal.h"
#include <errno.h>
#include <string.h>

/* littlefs 的错误码本身就是 -errno（lfs.h 的 `enum lfs_error`），原样透传；正数
 * 返回（不该出现）不猜成成功。 */
static int32_t littlefs_result(int err)
{
    if (err < 0)
    {
        return (int32_t)err;
    }

    return -EIO;
}

/* mount 后自检：写一个已知内容（长度超过 inline_max，落 data block，于是 erase /
 * prog / read 都被真实走过），只读重开读回并逐字节校验。用专用槽位，不占用公开
 * handle；`LFS_NO_MALLOC` 下 per-file buffer 必须显式提供。 */
static int32_t littlefs_selftest(struct littlefs_state *state)
{
    static const uint8_t content[] = "KaleidOS littlefs selftest: prog/erase/read ok";
    uint8_t scratch[sizeof(content)];
    struct lfs_file_config *config = &state->selftest_config;
    int err;

    memset(config, 0, sizeof(*config));
    config->buffer = state->selftest_buffer;

    err = lfs_file_opencfg(&state->filesystem, &state->selftest_file, LITTLEFS_SELFTEST_PATH,
                           LFS_O_WRONLY | LFS_O_CREAT | LFS_O_TRUNC, config);
    if (err != 0)
    {
        return littlefs_result(err);
    }

    lfs_ssize_t written = lfs_file_write(&state->filesystem, &state->selftest_file, content,
                                         (lfs_size_t)(sizeof(content) - 1));
    err = lfs_file_close(&state->filesystem, &state->selftest_file);
    if (written < 0)
    {
        return littlefs_result((int)written);
    }
    if (err != 0)
    {
        return littlefs_result(err);
    }

    err = lfs_file_opencfg(&state->filesystem, &state->selftest_file, LITTLEFS_SELFTEST_PATH,
                           LFS_O_RDONLY, config);
    if (err != 0)
    {
        return littlefs_result(err);
    }

    lfs_ssize_t got = lfs_file_read(&state->filesystem, &state->selftest_file, scratch,
                                    (lfs_size_t)sizeof(scratch));
    err = lfs_file_close(&state->filesystem, &state->selftest_file);
    if (got < 0)
    {
        return littlefs_result((int)got);
    }
    if (err != 0)
    {
        return littlefs_result(err);
    }

    if ((size_t)got != sizeof(content) - 1 || memcmp(scratch, content, sizeof(content) - 1) != 0)
    {
        return -EIO;
    }

    LITTLEFS_LOG_LINE("[littlefs] selftest ok");
    return 0;
}

int32_t littlefs_mount(void *ctx)
{
    struct littlefs_state *state = ctx;

    if (state == NULL || !state->alive)
    {
        return -ENODEV;
    }

    if (state->mounted)
    {
        return 0; /* Already mounted */
    }

    /* block_count 必须在 lfs_mount / lfs_format 前填好（block_size = 512 =
     * sector，因此容量 sector 数就是 block 数）。 */
    uint64_t sectors = 0;
    int32_t result = littlefs_adapter_capacity(state, &sectors);
    if (result < 0)
    {
        return result;
    }

    /* lfs_size_t 是 u32：容量必须装得下，0 容量不是文件系统。 */
    if (sectors == 0 || sectors > UINT32_MAX)
    {
        return -ENODEV;
    }
    state->config.block_count = (lfs_size_t)sectors;

    int err = lfs_mount(&state->filesystem, &state->config);
    if (err != 0)
    {
        /* 未格式化（或不可识别）：格式化一次再挂载。 */
        err = lfs_format(&state->filesystem, &state->config);
        if (err != 0)
        {
            return littlefs_result(err);
        }

        err = lfs_mount(&state->filesystem, &state->config);
        if (err != 0)
        {
            return littlefs_result(err);
        }
    }

    result = littlefs_selftest(state);
    if (result < 0)
    {
        (void)lfs_unmount(&state->filesystem);
        return result;
    }

    state->mounted = 1;
    LITTLEFS_LOG_LINE("[littlefs] mount");
    return 0;
}

int32_t littlefs_unmount(void *ctx)
{
    struct littlefs_state *state = ctx;

    if (state == NULL || !state->alive)
    {
        return -ENODEV;
    }

    if (!state->mounted)
    {
        return 0; /* Already unmounted */
    }

    for (size_t i = 0; i < LITTLEFS_MAX_OPEN_FILES; ++i)
    {
        if (state->files[i].used)
        {
            return -EBUSY;
        }
    }

    int err = lfs_unmount(&state->filesystem);
    if (err != 0)
    {
        return littlefs_result(err);
    }

    state->mounted = 0;
    LITTLEFS_LOG_LINE("[littlefs] unmount");
    return 0;
}

int32_t littlefs_open(void *ctx, const char *path, uint32_t flags, uint64_t *out_handle)
{
    struct littlefs_state *state = ctx;

    if (state == NULL || path == NULL || out_handle == NULL)
    {
        return -EINVAL;
    }

    *out_handle = 0;

    if (!state->alive || !state->mounted)
    {
        return -ENODEV;
    }

    /* 只读服务：写 / 其它 flags 一律 -EROFS（不静默降级）。 */
    if (flags != KCOMP_FILESYSTEM_OPEN_READ)
    {
        return -EROFS;
    }

    // Find an available file slot
    int slot_index = -1;
    for (int i = 0; i < LITTLEFS_MAX_OPEN_FILES; i++)
    {
        if (!state->files[i].used)
        {
            slot_index = i;
            break;
        }
    }

    if (slot_index == -1)
    {
        return -EMFILE;
    }

    struct littlefs_file_slot *slot = &state->files[slot_index];

    /* LFS_NO_MALLOC：per-file cache buffer 显式提供；config 必须存活到 close。 */
    memset(&slot->config, 0, sizeof(slot->config));
    slot->config.buffer = slot->buffer;

    int err = lfs_file_opencfg(&state->filesystem, &slot->file, path, LFS_O_RDONLY,
                               &slot->config);
    if (err != 0)
    {
        return littlefs_result(err);
    }

    slot->used = 1;
    *out_handle = (uint64_t)slot_index + 1;
    LITTLEFS_LOG_LINE("[littlefs] open");

    return 0;
}

int32_t littlefs_read(void *ctx, uint64_t handle, uint8_t *buf, size_t len, size_t *out_read)
{
    struct littlefs_state *state = ctx;

    if (state == NULL || buf == NULL || out_read == NULL)
    {
        return -EINVAL;
    }

    *out_read = 0;

    if (!state->alive || !state->mounted)
    {
        return -ENODEV;
    }

    if (len == 0)
    {
        return 0;
    }

    if (handle == 0 || handle > LITTLEFS_MAX_OPEN_FILES)
    {
        return -EBADF;
    }

    struct littlefs_file_slot *slot = &state->files[handle - 1];
    if (!slot->used)
    {
        return -EBADF;
    }

    /* lfs_size_t 是 u32：单次请求按上限截断（littlefs 语义允许短读）。 */
    lfs_size_t request = len > (size_t)(lfs_size_t)-1 ? (lfs_size_t)-1 : (lfs_size_t)len;

    lfs_ssize_t actual = lfs_file_read(&state->filesystem, &slot->file, buf, request);
    if (actual < 0)
    {
        return littlefs_result((int)actual);
    }

    *out_read = (size_t)actual;
    LITTLEFS_LOG_LINE("[littlefs] read");
    return 0;
}

int32_t littlefs_close(void *ctx, uint64_t handle)
{
    struct littlefs_state *state = ctx;

    if (state == NULL || handle == 0 || handle > LITTLEFS_MAX_OPEN_FILES)
    {
        return -EBADF;
    }

    if (!state->alive || !state->mounted)
    {
        return -ENODEV;
    }

    struct littlefs_file_slot *slot = &state->files[handle - 1];
    if (!slot->used)
    {
        return -EBADF;
    }

    int err = lfs_file_close(&state->filesystem, &slot->file);
    if (err != 0)
    {
        return littlefs_result(err);
    }

    slot->used = 0;
    LITTLEFS_LOG_LINE("[littlefs] close");
    return 0;
}
