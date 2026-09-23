#include "kcomp.h"
#include "ff.h"
#include "diskio.h"
#include "diskio_kaleidos.h"
#include <errno.h>
#include <string.h>

#define FATFS_MAX_OPEN_FILES 8

/* create config（组合策略提供；Core 视为不透明字节）。
 *
 *   endpoint = block provider 的 opaque EndpointId——组合期由 composer 用
 *              `kcore_endpoint_lookup` 解析后交付；本组件**不做**全局名字发现，
 *              没有 endpoint 就没有块设备。
 *
 * `config_abi` 是布局指纹（8 字节 ASCII "FATFSCFG" 的大端读数）：对不上直接拒绝
 * 创建，不静默按空配置跑。composer 的镜像定义见
 * `os/components/block_chain/src/lib.rs`（同一布局、同一指纹）。 */
struct fatfs_create_config
{
    uint64_t endpoint;
};

#define FATFS_CREATE_CONFIG_ABI UINT64_C(0x4641544653434647)

struct fatfs_file_slot
{
    int used;
    FIL file;
};

struct fatfs_state
{
    FATFS filesystem;

    /* Core 在 create 里选定的调用绑定（机制藏在绑定内部）。 */
    struct kcomp_block_binding block_binding;

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
    if (result < 0 || bytes_read == 0)
    {
        kcore_log_line((const uint8_t *)"[fatfs] selftest: read failed\n",
                       sizeof("[fatfs] selftest: read failed\n") - 1);
    }
    else
    {
        kcore_log_line((const uint8_t *)"[fatfs] selftest: read ok\n",
                       sizeof("[fatfs] selftest: read ok\n") - 1);
        /* 内容原样打一行（不带额外前缀）：QEMU runner 逐字节比对 provider 生成的
         * 卷内容——"C consumer 经 Direct 拿到 Rust provider 的正确字节"的证据。 */
        kcore_log_line(buffer, bytes_read);
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
    if (out_state == NULL)
        return -EFAULT;

    *out_state = NULL;

    /* block endpoint 必须由组合策略经 create config 交付（本组件不做全局名字
     * 发现）；config_abi 对不上 = 布局不符，拒绝创建而不是猜。 */
    if (args == NULL || args->config_abi != FATFS_CREATE_CONFIG_ABI)
    {
        return -EINVAL;
    }

    if (args->config == NULL || args->config_len != sizeof(struct fatfs_create_config))
    {
        return -EINVAL;
    }

    const struct fatfs_create_config *config =
        (const struct fatfs_create_config *)args->config;

    struct fatfs_state *state = (struct fatfs_state *)kcore_heap_alloc(
        sizeof(struct fatfs_state), _Alignof(struct fatfs_state));
    if (state == NULL)
    {
        return -ENOMEM;
    }

    memset(state, 0, sizeof(struct fatfs_state));

    /* bind block endpoint：Core exact-compare contract + abi、校验存活，并按
     * (caller, provider) 执行域**一次性选定机制**（Direct / Gate）——组件只执行，
     * 不选择、也看不到机制。 */
    int32_t result = kcomp_block_bind(
        config->endpoint,
        KCOMP_BLOCK_DEVICE_CONTRACT,
        KCOMP_BLOCK_DEVICE_ABI,
        &state->block_binding);
    if (result < 0)
    {
        kcore_heap_dealloc((uint8_t *)state, sizeof(struct fatfs_state), _Alignof(struct fatfs_state));
        return result;
    }

    state->alive = 1;

    result = fatfs_disk_attach(&state->block_binding);
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
