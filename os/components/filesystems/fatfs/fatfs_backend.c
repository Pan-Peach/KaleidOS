/* fatfs_backend.c —— FatFs 的**业务后端**（只读 FAT 文件系统语义）。
 *
 * Direct 的 `#[repr(C)]` function table（fatfs.c）与 Gate 的扁平 method switch
 * （fatfs_service.c）调用**同一份**实现；业务代码不感知部署。每个业务方法成功时打
 * 一行 `[fatfs] <op>`——QEMU runner 用它对照 Gate 入口日志做差分断言。
 */
#include "kcomp.h"
#include "fatfs_internal.h"
#include <errno.h>

static int32_t fatfs_result(FRESULT result)
{
    switch (result)
    {
    case FR_OK:
        return 0;

    case FR_NO_FILE:
    case FR_NO_PATH:
        return -ENOENT;

    case FR_DISK_ERR:
    case FR_INT_ERR:
        return -EIO;

    case FR_INVALID_OBJECT:
        return -EBADF;

    case FR_DENIED:
        return -EACCES;

    case FR_WRITE_PROTECTED:
        return -EROFS;

    case FR_NOT_ENOUGH_CORE:
        return -ENOMEM;

    case FR_EXIST:
        return -EEXIST;

    case FR_TIMEOUT:
        return -ETIMEDOUT;

    case FR_LOCKED:
        return -EBUSY;

    case FR_TOO_MANY_OPEN_FILES:
        return -EMFILE;

    case FR_INVALID_NAME:
    case FR_INVALID_PARAMETER:
        return -EINVAL;

    case FR_NOT_READY:
    case FR_INVALID_DRIVE:
    case FR_NOT_ENABLED:
    case FR_NO_FILESYSTEM:
        return -ENODEV;

    default:
        return -EIO;
    }
}

int32_t fatfs_mount(void *ctx)
{
    struct fatfs_state *state = ctx;

    if (state == NULL || !state->alive)
    {
        return -ENODEV;
    }

    if (state->mounted)
    {
        return 0; /* Already mounted */
    }

    FRESULT result = f_mount(&state->filesystem, "0:", 1);
    if (result != FR_OK)
    {
        return fatfs_result(result);
    }

    state->mounted = 1;
    FATFS_LOG_LINE("[fatfs] mount");
    return 0;
}

int32_t fatfs_unmount(void *ctx)
{
    struct fatfs_state *state = ctx;

    if (state == NULL || !state->alive)
    {
        return -ENODEV;
    }

    if (!state->mounted)
    {
        return 0; /* Already unmounted */
    }

    for (size_t i = 0; i < FATFS_MAX_OPEN_FILES; ++i)
    {
        if (state->files[i].used)
        {
            return -EBUSY;
        }
    }

    FRESULT result = f_mount(NULL, "0:", 0);
    if (result != FR_OK)
    {
        return fatfs_result(result);
    }

    state->mounted = 0;
    FATFS_LOG_LINE("[fatfs] unmount");
    return 0;
}

int32_t fatfs_open(void *ctx, const char *path, uint32_t flags, uint64_t *out_handle)
{
    struct fatfs_state *state = ctx;

    if (state == NULL || path == NULL || out_handle == NULL)
    {
        return -EINVAL;
    }

    *out_handle = 0;

    if (!state->alive)
        return -ENODEV;

    if (!state->mounted)
    {
        return -ENODEV;
    }

    if (flags != KCOMP_FILESYSTEM_OPEN_READ)
    {
        return -EROFS;
    }

    // Find an available file slot
    int slot_index = -1;
    for (int i = 0; i < FATFS_MAX_OPEN_FILES; i++)
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

    FIL *file = &state->files[slot_index].file;
    FRESULT result = f_open(file, path, FA_READ);
    if (result != FR_OK)
    {
        return fatfs_result(result);
    }

    state->files[slot_index].used = 1;
    *out_handle = (uint64_t)slot_index + 1;
    FATFS_LOG_LINE("[fatfs] open");

    return 0;
}

int32_t fatfs_read(
    void *ctx,
    uint64_t handle,
    uint8_t *buf,
    size_t len,
    size_t *out_read)
{
    struct fatfs_state *state = ctx;

    if (state == NULL || buf == NULL || out_read == NULL)
        return -EINVAL;

    *out_read = 0;

    if (!state->alive || !state->mounted)
        return -ENODEV;

    if (len == 0)
        return 0;

    if (handle == 0 || handle > FATFS_MAX_OPEN_FILES)
        return -EBADF;

    struct fatfs_file_slot *slot = &state->files[handle - 1];
    if (!slot->used)
        return -EBADF;

    UINT request = len > (size_t)(UINT)-1
                       ? (UINT)-1
                       : (UINT)len;

    UINT actual = 0;
    FRESULT result = f_read(&slot->file, buf, request, &actual);
    if (result != FR_OK)
        return fatfs_result(result);

    *out_read = (size_t)actual;
    FATFS_LOG_LINE("[fatfs] read");
    return 0;
}

int32_t fatfs_close(void *ctx, uint64_t handle)
{
    struct fatfs_state *state = ctx;

    if (state == NULL || handle == 0 || handle > FATFS_MAX_OPEN_FILES)
        return -EBADF;

    if (!state->alive || !state->mounted)
        return -ENODEV;

    struct fatfs_file_slot *slot = &state->files[handle - 1];
    if (!slot->used)
        return -EBADF;

    FRESULT result = f_close(&slot->file);
    if (result != FR_OK)
        return fatfs_result(result);

    slot->used = 0;
    FATFS_LOG_LINE("[fatfs] close");
    return 0;
}
