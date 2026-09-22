#include "kcomp.h"
#include "ff.h"
#include "diskio.h"
#include "diskio_kaleidos.h"
#include <errno.h>
#include <string.h>

#define FATFS_MAX_OPEN_FILES 8

struct fatfs_file_slot
{
    int used;
    FIL file;
};

struct fatfs_state
{
    FATFS filesystem;

    const struct kcomp_block_device_api *block_device_api;
    void *block_ctx;

    uint64_t binding;
    uint64_t generation;

    int mounted;
    int alive;

    struct fatfs_file_slot files[FATFS_MAX_OPEN_FILES];
};

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

static int32_t fatfs_mount(void *ctx)
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
    return 0;
}

static int32_t fatfs_unmount(void *ctx)
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
    return 0;
}

static int32_t fatfs_open(void *ctx, const char *path, uint32_t flags, uint64_t *out_handle)
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

    return 0;
}

static int32_t fatfs_read(
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
    return 0;
}

static int32_t fatfs_close(void *ctx, uint64_t handle)
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
    return 0;
}

static const struct kcomp_filesystem_api fatfs_api = {
    .mount = fatfs_mount,
    .unmount = fatfs_unmount,
    .open = fatfs_open,
    .close = fatfs_close,
    .read = fatfs_read,
};

#ifdef FATFS_SELFTEST

/* 临时端到端测试；真正的 FatFs 调用在 task context 中发生。 */
static void fatfs_selftest_task(void *arg)
{
    struct fatfs_state *state = arg;
    uint64_t handle = 0;
    uint8_t buffer[512];
    size_t bytes_read = 0;
    int32_t result;

    result = fatfs_mount(state);
    if (result < 0)
    {
        kcore_log_line((const uint8_t *)"[fatfs] selftest: mount failed\n",
                       sizeof("[fatfs] selftest: mount failed\n") - 1);
        goto exit;
    }

    result = fatfs_open(state, "0:/HELLO.TXT", KCOMP_FILESYSTEM_OPEN_READ, &handle);
    if (result < 0)
    {
        kcore_log_line((const uint8_t *)"[fatfs] selftest: open failed\n",
                       sizeof("[fatfs] selftest: open failed\n") - 1);
        goto unmount;
    }

    result = fatfs_read(state, handle, buffer, sizeof(buffer), &bytes_read);
    if (result < 0)
    {
        kcore_log_line((const uint8_t *)"[fatfs] selftest: read failed\n",
                       sizeof("[fatfs] selftest: read failed\n") - 1);
    }
    else
    {
        (void)bytes_read;
        kcore_log_line((const uint8_t *)"[fatfs] selftest: read ok\n",
                       sizeof("[fatfs] selftest: read ok\n") - 1);
    }

    if (fatfs_close(state, handle) < 0)
    {
        kcore_log_line((const uint8_t *)"[fatfs] selftest: close failed\n",
                       sizeof("[fatfs] selftest: close failed\n") - 1);
    }

unmount:
    if (fatfs_unmount(state) < 0)
    {
        kcore_log_line((const uint8_t *)"[fatfs] selftest: unmount failed\n",
                       sizeof("[fatfs] selftest: unmount failed\n") - 1);
    }

exit:
    /* kcore_task_exit 不会把控制权返回给这个 task。 */
    (void)kcore_task_exit();
    for (;;) {
    }
}

#endif

const uint64_t kcomp_abi = UINT64_C(0x4B434F4D50414249);

int32_t kcomp_instance_create(
    const struct KcompCreateArgs *args,
    void **out_state)
{
    (void)args;

    if (out_state == NULL)
        return -EFAULT;

    *out_state = NULL;

    struct fatfs_state *state = (struct fatfs_state *)kcore_heap_alloc(
        sizeof(struct fatfs_state), _Alignof(struct fatfs_state));
    if (state == NULL)
    {
        return -ENOMEM;
    }

    memset(state, 0, sizeof(struct fatfs_state));

    size_t block_api_raw = 0;
    size_t block_ctx_raw = 0;

    // Register the filesystem API with the core
    int32_t result = kcore_interface_bind(
        (const uint8_t *)KCOMP_BLOCK_DEVICE_NAME,
        sizeof(KCOMP_BLOCK_DEVICE_NAME) - 1,
        KCOMP_IFACE_DEVICE,
        KCOMP_BLOCK_DEVICE_ABI,
        &state->binding,
        &block_api_raw,
        &block_ctx_raw,
        &state->generation);
    if (result < 0)
    {
        kcore_heap_dealloc((uint8_t *)state, sizeof(struct fatfs_state), _Alignof(struct fatfs_state));
        return result;
    }

    state->block_device_api = (const struct kcomp_block_device_api *)(uintptr_t)block_api_raw;
    state->block_ctx = (void *)(uintptr_t)block_ctx_raw;
    state->alive = 1;

    result = fatfs_disk_attach(state->block_device_api, state->block_ctx);
    if (result < 0)
    {
        kcore_heap_dealloc((uint8_t *)state, sizeof(struct fatfs_state), _Alignof(struct fatfs_state));
        return result;
    }

    result = kcore_interface_publish(
        (const uint8_t *)KCOMP_FILESYSTEM_NAME,
        sizeof(KCOMP_FILESYSTEM_NAME) - 1,
        KCOMP_IFACE_SERVICE,
        KCOMP_FILESYSTEM_ABI,
        &fatfs_api,
        state);

    if (result < 0)
    {
        fatfs_disk_detach();
        kcore_heap_dealloc((uint8_t *)state, sizeof(struct fatfs_state), _Alignof(struct fatfs_state));
        return result;
    }


    *out_state = state;

#ifdef FATFS_SELFTEST
    {
        uint32_t selftest_task = 0;

        result = kcore_task_create(
            fatfs_selftest_task,
            state,
            &selftest_task);
        if (result < 0)
        {
            fatfs_disk_detach();
            kcore_heap_dealloc(
                (uint8_t *)state,
                sizeof(struct fatfs_state),
                _Alignof(struct fatfs_state));
            *out_state = NULL;
            return result;
        }

        result = kcore_task_start(selftest_task);
        if (result < 0)
        {
            /* task record 已经存在，state 按 phase 1 规则保留。 */
            fatfs_disk_detach();
            return result;
        }
    }
#endif

    return 0;
}

int32_t kcomp_instance_destroy(void *opaque_state)
{
    struct fatfs_state *state = opaque_state;

    if (state == NULL)
        return 0;

    state->alive = 0;

    fatfs_disk_detach();

    /* 已发布的 ctx 可能仍被消费者缓存；phase 1 只逻辑停止，不回收 state。 */
    return 0;
}
