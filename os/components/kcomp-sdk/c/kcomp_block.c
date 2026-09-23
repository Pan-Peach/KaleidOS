/* kcomp_block.c —— block.device 调用绑定的实现（不透明绑定的内部表示）。
 *
 * 随每个 C 组件私有携带（tools/build-kcomp-c.sh 编译 SDK C 运行时的全部源文件）；
 * 机制由
 * Core 在 bind 时选定，本文件只**实现**两条执行路径：
 *
 *   DIRECT：直调 provider 的 #[repr(C)] function table（api + ctx）——零 Core
 *           介入、零分配、零打包（稳态）；
 *   GATE  ：经 kcore_endpoint_call 的 Core call gate（扁平线格式与 DIRECT 一致）。
 *
 * 组件代码只看到 `struct kcomp_block_binding` 与统一调用，**看不到机制**。
 */
#include "kcomp.h"
#include "kcomp_block.h"
#include <errno.h>

/* 不透明绑定的内部表示。header 的 `opaque[4]` 至少这么大、对齐至少这么强
 * （下面的静态断言钉住；RV32 上内部表示更小，只用前缀）。 */
struct kcomp_block_binding_internal {
    uint32_t mechanism; /* KCORE_ENDPOINT_MECHANISM_* */
    uint32_t reserved;  /* 保持 8 字节对齐，无语义 */
    uint64_t endpoint;  /* GATE：opaque EndpointId（call-gate handle） */
    const struct kcomp_block_device_api *api; /* DIRECT：provider function table */
    void *ctx;                                /* DIRECT：provider opaque state */
};

_Static_assert(sizeof(struct kcomp_block_binding_internal) <= sizeof(struct kcomp_block_binding),
               "kcomp_block_binding too small for its internal representation");
_Static_assert(_Alignof(struct kcomp_block_binding_internal) <= _Alignof(struct kcomp_block_binding),
               "kcomp_block_binding alignment drift");

static struct kcomp_block_binding_internal *binding_mut(struct kcomp_block_binding *binding)
{
    return (struct kcomp_block_binding_internal *)(void *)binding;
}

static const struct kcomp_block_binding_internal *binding_ref(
    const struct kcomp_block_binding *binding)
{
    return (const struct kcomp_block_binding_internal *)(const void *)binding;
}

/* 内部失败（Core 回复违反契约 / 绑定未初始化）的传输状态：不猜测、不降级。 */
static struct kcomp_call_result error_result(int32_t transport)
{
    struct kcomp_call_result result;

    result.transport = transport;
    result.method = 0;
    return result;
}

int32_t kcomp_block_bind(uint64_t endpoint, uint64_t contract, uint64_t abi,
                         struct kcomp_block_binding *out_binding)
{
    uint32_t mechanism = 0;
    size_t api_raw = 0;
    size_t ctx_raw = 0;
    int32_t status;

    if (out_binding == NULL) {
        return -EINVAL;
    }

    status = kcore_endpoint_bind(endpoint, contract, abi, &mechanism, &api_raw, &ctx_raw);
    if (status < 0) {
        return status;
    }

    struct kcomp_block_binding_internal *binding = binding_mut(out_binding);
    binding->reserved = 0;

    if (mechanism == KCORE_ENDPOINT_MECHANISM_DIRECT) {
        if (api_raw == 0) {
            /* Core 契约：Direct 必带非空 function table。 */
            return -EPROTO;
        }
        binding->mechanism = mechanism;
        binding->endpoint = 0;
        binding->api = (const struct kcomp_block_device_api *)(uintptr_t)api_raw;
        binding->ctx = (void *)(uintptr_t)ctx_raw;
        return 0;
    }

    if (mechanism == KCORE_ENDPOINT_MECHANISM_GATE) {
        /* Gate 不携带裸 function table：只留 opaque EndpointId。 */
        binding->mechanism = mechanism;
        binding->endpoint = endpoint;
        binding->api = NULL;
        binding->ctx = NULL;
        return 0;
    }

    /* 未知机制编码 = Core 回复违反契约（不猜测、不降级成 Direct）。 */
    return -EPROTO;
}

struct kcomp_call_result kcomp_block_read(const struct kcomp_block_binding *binding,
                                          uint64_t lba, void *output, size_t output_len)
{
    if (binding == NULL) {
        return error_result(-EINVAL);
    }
    const struct kcomp_block_binding_internal *b = binding_ref(binding);

    if (b->mechanism == KCORE_ENDPOINT_MECHANISM_DIRECT) {
        if (b->api == NULL || b->api->read == NULL) {
            return error_result(-EPROTO);
        }
        /* Direct 没有传输层：transport = 0，method 就是 table 的返回。 */
        struct kcomp_call_result result;
        result.transport = 0;
        result.method = b->api->read(b->ctx, lba, (uint8_t *)output, output_len);
        return result;
    }

    if (b->mechanism == KCORE_ENDPOINT_MECHANISM_GATE) {
        uint8_t args[KCOMP_BLOCK_LBA_LEN];
        struct kcomp_call_result result;

        for (size_t i = 0; i < KCOMP_BLOCK_LBA_LEN; i++) {
            args[i] = KCOMP_BLOCK_LBA_BYTE(lba, i);
        }
        result.method = 0;
        result.transport = kcore_endpoint_call(b->endpoint, KCOMP_BLOCK_METHOD_READ, args,
                                               sizeof(args), NULL, 0, (uint8_t *)output,
                                               output_len, &result.method);
        return result;
    }

    return error_result(-EINVAL);
}

struct kcomp_call_result kcomp_block_capacity(const struct kcomp_block_binding *binding,
                                              uint64_t *out_sectors)
{
    if (binding == NULL || out_sectors == NULL) {
        return error_result(-EINVAL);
    }
    const struct kcomp_block_binding_internal *b = binding_ref(binding);

    if (b->mechanism == KCORE_ENDPOINT_MECHANISM_DIRECT) {
        if (b->api == NULL || b->api->capacity_sectors == NULL) {
            return error_result(-EPROTO);
        }
        struct kcomp_call_result result;
        result.transport = 0;
        result.method = 0;
        *out_sectors = b->api->capacity_sectors(b->ctx);
        return result;
    }

    if (b->mechanism == KCORE_ENDPOINT_MECHANISM_GATE) {
        uint8_t reply[KCOMP_BLOCK_CAPACITY_LEN];
        struct kcomp_call_result result;

        result.method = 0;
        result.transport = kcore_endpoint_call(b->endpoint, KCOMP_BLOCK_METHOD_CAPACITY, NULL, 0,
                                               NULL, 0, reply, sizeof(reply), &result.method);
        if (result.transport == 0 && result.method == 0) {
            uint64_t value = 0;
            for (size_t i = 0; i < KCOMP_BLOCK_CAPACITY_LEN; i++) {
                value |= ((uint64_t)reply[i]) << (8u * (uint32_t)i);
            }
            *out_sectors = value;
        }
        return result;
    }

    return error_result(-EINVAL);
}
