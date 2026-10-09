/* fatfs.c —— FatFs 组件的**生命周期**：create / destroy + endpoint 发布。
 *
 * 业务后端（只读 FAT 文件系统语义）在 fatfs_backend.c；Gate 的扁平 method switch
 * 在 fatfs_service.c。本文件把两者接起来：
 *
 *   create  → alloc state → bind block endpoint（Core 选定机制）→ attach 磁盘胶水
 *           → 发布 filesystem endpoint（api/ctx = Direct 的 function table，
 *             port = Gate token）
 *   destroy → 逻辑停止（不回收 state）
 */
#include "kcomp.h"
#include "diskio_kaleidos.h"
#include "fatfs_internal.h"
#include <errno.h>
#include <string.h>

/* create config（组合策略提供；Core 视为不透明字节）。
 *
 *   endpoint = block provider 的 opaque EndpointId——组合期由 composer 用
 *              `kcore_endpoint_lookup` 解析后交付；本组件**不做**全局名字发现，
 *              没有 endpoint 就没有块设备。
 *
 * `config_abi` 是布局指纹（8 字节 ASCII "FATFSCFG" 的大端读数）：对不上直接拒绝
 * 创建，不静默按空配置跑。组合方（init / CoreTest）的 create config 定义见
 * `os/components/tests/core_test/src/runtime/filesystem.rs`（同一布局、同一指纹）。 */
struct fatfs_create_config
{
    uint64_t endpoint;
};

#define FATFS_CREATE_CONFIG_ABI UINT64_C(0x4641544653434647)

/* Direct transport：endpoint 发布时作为 api/ctx 交付的 `#[repr(C)]` function table。
 * 同一份业务实现也服务 Gate（fatfs_service.c 的扁平 method switch）。 */
static const struct kcomp_filesystem_api fatfs_api = {
    .mount = fatfs_mount,
    .unmount = fatfs_unmount,
    .open = fatfs_open,
    .close = fatfs_close,
    .read = fatfs_read,
    .root = fatfs_root,
    .lookup = fatfs_lookup,
    .node_info = fatfs_node_info,
};

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

    /* 取一段 backing（首次交付零初始化）；失败 = -errno。构造期清理由组件负责。 */
    struct kcore_memory_view state_region;
    if (kcomp_mem_alloc(&state_region, sizeof(struct fatfs_state),
                        _Alignof(struct fatfs_state)) < 0)
    {
        return -ENOMEM;
    }
    struct fatfs_state *state = (struct fatfs_state *)(uintptr_t)state_region.base;

    /* bind block endpoint：Core exact-compare contract + abi、校验存活，并按
     * (caller, provider) 执行域**一次性选定机制**（Direct / Gate）——组件只执行，
     * 不选择、也看不到机制。 */
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

    result = fatfs_disk_attach(&state->block_binding);
    if (result < 0)
    {
        kcomp_mem_free(&state_region);
        return result;
    }

    /* 发布 filesystem endpoint（staged：Core 在 create 返回 0 后原子提交）：
     * port_name = 契约名（单例固定名，组合策略据此发现），contract = 契约身份，
     * port = 本 provider 的 Gate dispatch token；api/ctx = Direct 的 function
     * table + state（Core 只存、bind 时按机制交付）。两条 transport 都提供，
     * **不选择**。 */
    result = kcore_endpoint_publish(
        (const uint8_t *)KCOMP_FILESYSTEM_NAME,
        sizeof(KCOMP_FILESYSTEM_NAME) - 1,
        KCOMP_FILESYSTEM_CONTRACT,
        KCOMP_IFACE_SERVICE,
        KCOMP_FILESYSTEM_ABI,
        FATFS_PORT,
        &fatfs_api,
        state);

    if (result < 0)
    {
        fatfs_disk_detach();
        kcomp_mem_free(&state_region);
        return result;
    }

    *out_state = state;
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

    /* endpoint / binding 的 ctx 可能仍被消费者缓存；只逻辑停止，不回收 state。 */
    fatfs_leave(state);
    FATFS_LOG_LINE("[fatfs] destroy");
    return 0;
}
