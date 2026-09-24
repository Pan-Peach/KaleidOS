/* fs_consumer —— filesystem endpoint 的测试消费者（C）。
 *
 * 它只做组合策略允许的事：从 **create config** 拿到 composer 解析好的 opaque
 * EndpointId（**不做**全局名字发现），`kcomp_filesystem_bind`（Core 在 bind 时
 * 选定机制，藏在绑定内部），然后经统一包装 `kcomp_filesystem_mount` /
 * `open` / `read` 读 `HELLO.TXT`，并逐字节校验内容：
 *
 *   - `[fs_consumer] mount ok` / `open ok` / `read ok` —— 成功证据；
 *   - 原始内容行（无前缀）—— QEMU runner 逐字节比对 provider 的合成卷；
 *   - 任一步失败打 `[fs_consumer] FAIL: ...`（runner 的致命标记）。
 *
 * create config（组合策略提供；Core 视为不透明字节）：
 *   endpoint = fatfs 的 filesystem EndpointId；指纹 = 8 字节 ASCII "FSCONSUM"。
 * composer 的镜像定义见 `os/components/block_chain/src/lib.rs`（同布局、同指纹）。
 *
 * `read` 的缓冲区布局与线格式一致：前 KCOMP_FILESYSTEM_READ_HEADER_LEN (8) 字节是
 * LE u64 实际长度头，数据从 offset 8 开始（见 kcomp_filesystem.h）。
 */
#include "kcomp.h"
#include <errno.h>
#include <string.h>

struct fs_consumer_create_config
{
    uint64_t endpoint;
};

#define FS_CONSUMER_CREATE_CONFIG_ABI UINT64_C(0x4653434F4E53554D) /* "FSCONSUM" */

/* ram_blk 的合成 FAT12 卷里 HELLO.TXT 的内容（runner 也断言同一行）。 */
static const uint8_t expected_content[] = "KALEIDOS BLOCK CHAIN OK";

struct fs_consumer_state
{
    struct kcomp_filesystem_binding binding;
};

#define FS_CONSUMER_LOG_LINE(text) kcore_log_line((const uint8_t *)(text), sizeof(text) - 1)

/* 把调用结果收成 0 / -errno：传输失败优先，传输成功才看 provider 的方法状态。 */
static int32_t result_code(struct kcomp_call_result result)
{
    if (result.transport < 0)
    {
        return result.transport;
    }
    return result.method;
}

static void fs_consumer_task(void *arg)
{
    struct fs_consumer_state *state = arg;
    uint64_t handle = 0;
    size_t actual = 0;
    int32_t result;

    result = result_code(kcomp_filesystem_mount(&state->binding));
    if (result < 0)
    {
        FS_CONSUMER_LOG_LINE("[fs_consumer] FAIL: mount");
        goto exit;
    }
    FS_CONSUMER_LOG_LINE("[fs_consumer] mount ok");

    result = result_code(kcomp_filesystem_open(
        &state->binding, "0:/HELLO.TXT", KCOMP_FILESYSTEM_OPEN_READ, &handle));
    if (result < 0)
    {
        FS_CONSUMER_LOG_LINE("[fs_consumer] FAIL: open");
        goto unmount;
    }
    FS_CONSUMER_LOG_LINE("[fs_consumer] open ok");

    /* output = 8 字节长度头 + 数据区（数据容量 = sizeof(frame) - 8）。 */
    uint8_t frame[KCOMP_FILESYSTEM_READ_HEADER_LEN + 64];
    result = result_code(
        kcomp_filesystem_read(&state->binding, handle, frame, sizeof(frame), &actual));
    if (result < 0)
    {
        FS_CONSUMER_LOG_LINE("[fs_consumer] FAIL: read");
    }
    else if (actual != sizeof(expected_content) - 1 ||
             memcmp(frame + KCOMP_FILESYSTEM_READ_HEADER_LEN, expected_content, actual) != 0)
    {
        FS_CONSUMER_LOG_LINE("[fs_consumer] FAIL: content mismatch");
    }
    else
    {
        FS_CONSUMER_LOG_LINE("[fs_consumer] read ok");
        /* 原始内容行（无前缀）：runner 逐字节比对 provider 生成的卷内容。 */
        kcore_log_line(frame + KCOMP_FILESYSTEM_READ_HEADER_LEN, actual);
    }

    if (result_code(kcomp_filesystem_close(&state->binding, handle)) < 0)
    {
        FS_CONSUMER_LOG_LINE("[fs_consumer] FAIL: close");
    }
    else
    {
        FS_CONSUMER_LOG_LINE("[fs_consumer] close ok");
    }

unmount:
    if (result_code(kcomp_filesystem_unmount(&state->binding)) < 0)
    {
        FS_CONSUMER_LOG_LINE("[fs_consumer] FAIL: unmount");
    }
    else
    {
        FS_CONSUMER_LOG_LINE("[fs_consumer] unmount ok");
    }

exit:
    /* kcore_task_exit 不会把控制权返回给这个 task。 */
    (void)kcore_task_exit();
    for (;;)
    {
    }
}

const uint64_t kcomp_abi = UINT64_C(0x4B434F4D50414249);

int32_t kcomp_instance_create(
    const struct KcompCreateArgs *args,
    void **out_state)
{
    if (out_state == NULL)
        return -EFAULT;

    *out_state = NULL;

    /* filesystem endpoint 必须由组合策略经 create config 交付（本组件不做全局
     * 名字发现）；config_abi 对不上 = 布局不符，拒绝创建而不是猜。 */
    if (args == NULL || args->config_abi != FS_CONSUMER_CREATE_CONFIG_ABI)
    {
        return -EINVAL;
    }

    if (args->config == NULL || args->config_len != sizeof(struct fs_consumer_create_config))
    {
        return -EINVAL;
    }

    const struct fs_consumer_create_config *config =
        (const struct fs_consumer_create_config *)args->config;

    /* 取一段 backing（首次交付零初始化）；失败 = -errno。构造期清理由组件负责。 */
    struct kcore_memory_view state_region;
    if (kcomp_mem_alloc(&state_region, sizeof(struct fs_consumer_state),
                        _Alignof(struct fs_consumer_state)) < 0)
    {
        return -ENOMEM;
    }
    struct fs_consumer_state *state = (struct fs_consumer_state *)(uintptr_t)state_region.base;

    /* bind filesystem endpoint：Core exact-compare contract + abi、校验存活，并按
     * (caller, provider) 执行域**一次性选定机制**（Direct / Gate）——组件只执行，
     * 不选择、也看不到机制。 */
    int32_t result = kcomp_filesystem_bind(
        config->endpoint,
        KCOMP_FILESYSTEM_CONTRACT,
        KCOMP_FILESYSTEM_ABI,
        &state->binding);
    if (result < 0)
    {
        kcomp_mem_free(&state_region);
        return result;
    }

    /* 真正的文件系统调用在 task context 中发生（阻塞语义 / 与 create 边界解耦）。 */
    uint32_t task = 0;
    result = kcore_task_create(fs_consumer_task, state, &task);
    if (result < 0)
    {
        kcomp_mem_free(&state_region);
        return result;
    }

    result = kcore_task_start(task);
    if (result < 0)
    {
        /* task record 已经存在，state 按 phase 1 规则保留。 */
        return result;
    }

    *out_state = state;
    return 0;
}

int32_t kcomp_instance_destroy(void *opaque_state)
{
    struct fs_consumer_state *state = opaque_state;

    if (state == NULL)
        return 0;

    /* 绑定内部可能仍被 task 使用；phase 1 只逻辑停止，不回收 state。 */
    FS_CONSUMER_LOG_LINE("[fs_consumer] destroy");
    return 0;
}
