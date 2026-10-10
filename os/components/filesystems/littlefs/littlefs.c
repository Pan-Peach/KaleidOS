/* littlefs lifecycle: bind exact IPC Block endpoint, publish IPC-only FS, start owned Server. */
#include "kcomp.h"
#include "lfs_adapter.h"
#include "littlefs_internal.h"
#include "kcomp_ipc.h"
#include <errno.h>

/* create config（组合策略提供；Core 视为不透明字节）。
 *
 *   endpoint = block provider 的 opaque EndpointId——组合期由 composer 用
 *              `kcore_endpoint_lookup` 解析后交付；本组件**不做**全局名字发现，
 *              没有 endpoint 就没有块设备。
 *
 * `config_abi` 是布局指纹（8 字节 ASCII "LITTLEIP" 的大端读数）：对不上直接拒绝
 * 创建，不静默按空配置跑。 */
struct littlefs_create_config
{
    uint64_t endpoint;
    uint32_t control;
    uint32_t reserved;
};

#define LITTLEFS_CREATE_CONFIG_ABI KCOMP_LITTLEFS_CREATE_CONFIG_ABI

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
    if (args == NULL || args->config_abi != LITTLEFS_CREATE_CONFIG_ABI)
    {
        return -EINVAL;
    }

    if (args->config == NULL || args->config_len != sizeof(struct littlefs_create_config))
    {
        return -EINVAL;
    }

    const uint8_t *bytes = args->config;
    struct littlefs_create_config config = {kcomp_ipc_u64(bytes), kcomp_ipc_u32(bytes+8), kcomp_ipc_u32(bytes+12)};
    if (config.reserved || !config.control) return -EINVAL;

    /* 取一段 backing（首次交付零初始化）；失败 = -errno。构造期清理由组件负责。 */
    struct kcore_memory_view state_region;
    if (kcomp_mem_alloc(&state_region, sizeof(struct littlefs_state),
                        _Alignof(struct littlefs_state)) < 0)
    {
        return -ENOMEM;
    }
    struct littlefs_state *state = (struct littlefs_state *)(uintptr_t)state_region.base;

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

    /* 填 lfs_config：回调 + 几何参数 + 显式缓冲区（LFS_NO_MALLOC，多实例互不
     * 共享）。block_count 留到 mount 时按设备容量填。 */
    littlefs_adapter_init(state);

    state->alive = 1;
    state->control = config.control;

    /* Staged publication commits only after successful create. */
    result = kcore_endpoint_publish(
        (const uint8_t *)KCOMP_FILESYSTEM_NAME,
        sizeof(KCOMP_FILESYSTEM_NAME) - 1,
        KCOMP_FILESYSTEM_CONTRACT,
        KCOMP_IFACE_SERVICE,
        KCOMP_FILESYSTEM_ABI,
        0, NULL, NULL);

    if (result < 0)
    {
        kcomp_mem_free(&state_region);
        return result;
    }

    *out_state = state;
    uint32_t task = 0;
    result = kcore_task_create(littlefs_server, state, &task);
    if (!result) result = kcore_task_start(task);
    /* Once a Task retains arg, failed Native backing remains resident. */
    if (result) return result;
    LITTLEFS_LOG_LINE("[littlefs] endpoint published");

    return 0;
}

int32_t kcomp_instance_destroy(void *opaque_state)
{
    struct littlefs_state *state = opaque_state;

    if (state == NULL)
        return 0;

    if (!littlefs_enter(state))
        return -EBUSY;
    state->alive = 0;

    /* endpoint / binding 的 ctx 可能仍被消费者缓存，open 文件与 lfs_t 都在 state
     * 里；只逻辑停止，不回收 state。 */
    littlefs_leave(state);
    LITTLEFS_LOG_LINE("[littlefs] destroy");
    return 0;
}
