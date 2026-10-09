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

static struct fatfs_file_slot *fatfs_find(struct fatfs_state *state, uint64_t handle)
{
    if (handle != 0) {
        for (size_t i = 0; i < FATFS_MAX_OPEN_FILES; i++) {
            if (state->files[i].handle == handle)
                return &state->files[i];
        }
    }
    return NULL;
}

static int32_t fatfs_mount_locked(void *ctx)
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

static int32_t fatfs_unmount_locked(void *ctx)
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
        if (state->files[i].handle)
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

static int32_t fatfs_open_locked(void *ctx, const char *path, uint32_t flags, uint64_t *out_handle)
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

    if (state->last_handle == UINT64_MAX)
        return -EOVERFLOW;

    // Find an available file slot
    int slot_index = -1;
    for (int i = 0; i < FATFS_MAX_OPEN_FILES; i++)
    {
        if (!state->files[i].handle)
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

    *out_handle = ++state->last_handle;
    state->files[slot_index].handle = *out_handle;
    FATFS_LOG_LINE("[fatfs] open");

    return 0;
}

static int32_t fatfs_read_locked(
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

    struct fatfs_file_slot *slot = fatfs_find(state, handle);
    if (slot == NULL)
        return -EBADF;

    if (len == 0)
        return 0;

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

static int32_t fatfs_close_locked(void *ctx, uint64_t handle)
{
    struct fatfs_state *state = ctx;

    if (state == NULL)
        return -EBADF;

    if (!state->alive || !state->mounted)
        return -ENODEV;

    struct fatfs_file_slot *slot = fatfs_find(state, handle);
    if (slot == NULL)
        return -EBADF;

    FRESULT result = f_close(&slot->file);
    if (result != FR_OK)
        return fatfs_result(result);

    slot->handle = 0;
    FATFS_LOG_LINE("[fatfs] close");
    return 0;
}

int32_t fatfs_mount(void *ctx)
{
    struct fatfs_state *state = ctx;
    if (state == NULL)
        return -EINVAL;
    if (!fatfs_enter(state))
        return -EBUSY;
    int32_t result = fatfs_mount_locked(ctx);
    fatfs_leave(state);
    return result;
}

int32_t fatfs_unmount(void *ctx)
{
    struct fatfs_state *state = ctx;
    if (state == NULL)
        return -EINVAL;
    if (!fatfs_enter(state))
        return -EBUSY;
    int32_t result = fatfs_unmount_locked(ctx);
    fatfs_leave(state);
    return result;
}

int32_t fatfs_open(void *ctx, const char *path, uint32_t flags, uint64_t *out_handle)
{
    struct fatfs_state *state = ctx;
    if (state == NULL)
        return -EINVAL;
    if (!fatfs_enter(state))
        return -EBUSY;
    int32_t result = fatfs_open_locked(ctx, path, flags, out_handle);
    fatfs_leave(state);
    return result;
}

int32_t fatfs_close(void *ctx, uint64_t handle)
{
    struct fatfs_state *state = ctx;
    if (state == NULL)
        return -EINVAL;
    if (!fatfs_enter(state))
        return -EBUSY;
    int32_t result = fatfs_close_locked(ctx, handle);
    fatfs_leave(state);
    return result;
}

int32_t fatfs_read(void *ctx, uint64_t handle, uint8_t *buf, size_t len, size_t *out_read)
{
    struct fatfs_state *state = ctx;
    if (state == NULL)
        return -EINVAL;
    if (!fatfs_enter(state))
        return -EBUSY;
    int32_t result = fatfs_read_locked(ctx, handle, buf, len, out_read);
    fatfs_leave(state);
    return result;
}
