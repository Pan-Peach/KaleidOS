/* fatfs lifecycle: bind exact IPC Block endpoint, publish IPC-only FS, start owned Server. */
#include "kcomp.h"
#include "diskio_kaleidos.h"
#include "fatfs_internal.h"
#include "kcomp_ipc.h"
#include <errno.h>
#include <string.h>

/* create config（组合策略提供；Core 视为不透明字节）。
 *
 *   endpoint = block provider 的 opaque EndpointId——组合期由 composer 用
 *              `kcore_endpoint_lookup` 解析后交付；本组件**不做**全局名字发现，
 *              没有 endpoint 就没有块设备。
 *
 * `config_abi` 是布局指纹（8 字节 ASCII "FATFSIPC" 的大端读数）：对不上直接拒绝
 * 创建，不静默按空配置跑。组合方（init / CoreTest）的 create config 定义见
 * `os/components/tests/core_test/src/runtime/filesystem.rs`（同一布局、同一指纹）。 */
struct fatfs_create_config
{
    uint64_t endpoint;
    uint32_t control;
    uint32_t reserved;
};

#define FATFS_CREATE_CONFIG_ABI KCOMP_FATFS_CREATE_CONFIG_ABI

const uint64_t kcomp_abi = KCOMP_ABI;

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

    /* Opaque byte payloads need not have uint64_t alignment. */
    struct fatfs_create_config config;
    memcpy(&config, args->config, sizeof(config));
    const uint8_t *bytes = args->config;
    config.endpoint = kcomp_ipc_u64(bytes);
    config.control = kcomp_ipc_u32(bytes + 8);
    config.reserved = kcomp_ipc_u32(bytes + 12);
    if (config.reserved || !config.control) return -EINVAL;

    /* 取一段 backing（首次交付零初始化）；失败 = -errno。构造期清理由组件负责。 */
    struct kcore_memory_view state_region;
    if (kcomp_mem_alloc(&state_region, sizeof(struct fatfs_state),
                        _Alignof(struct fatfs_state)) < 0)
    {
        return -ENOMEM;
    }
    struct fatfs_state *state = (struct fatfs_state *)(uintptr_t)state_region.base;

    /* Binding verifies the exact IPC endpoint; composer grants send rights. */
    int32_t result = kcomp_block_bind(
        config.endpoint,
        KCOMP_BLOCK_DEVICE_CONTRACT,
        KCOMP_BLOCK_DEVICE_ABI,
        &state->block_binding);
    if (result < 0)
    {
        kcomp_mem_free(&state_region);
        return result;
    }

    state->alive = 1;
    state->control = config.control;

    result = fatfs_disk_attach(&state->block_binding);
    if (result < 0)
    {
        kcomp_mem_free(&state_region);
        return result;
    }

    /* Staged publication commits only after successful create. */
    result = kcore_endpoint_publish(
        (const uint8_t *)KCOMP_FILESYSTEM_NAME,
        sizeof(KCOMP_FILESYSTEM_NAME) - 1,
        KCOMP_FILESYSTEM_CONTRACT,
        KCOMP_IFACE_SERVICE,
        KCOMP_FILESYSTEM_ABI,
        0,
        NULL,
        NULL);

    if (result < 0)
    {
        fatfs_disk_detach();
        kcomp_mem_free(&state_region);
        return result;
    }

    *out_state = state;
    uint32_t task = 0;
    result = kcore_task_create(fatfs_server, state, &task);
    if (result == 0) result = kcore_task_start(task);
    if (result < 0) {
        state->alive = 0;
        fatfs_disk_detach();
        /* Created Task may retain its arg; backing remains resident on failure. */
        return result;
    }
    FATFS_LOG_LINE("[fatfs] endpoint published");

    return 0;
}

int32_t kcomp_instance_destroy(void *opaque_state)
{
    struct fatfs_state *state = opaque_state;

    if (state == NULL)
        return 0;

    if (!fatfs_enter(state))
        return -EBUSY;
    state->alive = 0;

    fatfs_disk_detach();

    /* Published Native backing remains resident after logical retirement. */
    fatfs_leave(state);
    FATFS_LOG_LINE("[fatfs] destroy");
    return 0;
}
