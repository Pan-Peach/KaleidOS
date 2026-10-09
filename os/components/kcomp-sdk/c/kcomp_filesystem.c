/* kcomp_filesystem.c —— filesystem 调用绑定的实现（不透明绑定的内部表示）。
 *
 * 随每个 C 组件私有携带（tools/build-kcomp-c.sh 编译 SDK C 运行时的全部源文件）；
 * 机制由 Core 在 bind 时选定，本文件只**实现**两条执行路径：
 *
 *   DIRECT：直调 provider 的 #[repr(C)] function table（api + ctx）——零 Core
 *           介入、零分配、零打包（稳态）；
 *   GATE  ：经 kcore_endpoint_call 的 Core call gate（扁平线格式与 DIRECT 一致）。
 *
 * 组件代码只看到 `struct kcomp_filesystem_binding` 与统一调用，**看不到机制**。
 *
 * read 接受普通数据缓冲区；Gate 的长度头和最多 512 字节 scratch 留在 SDK。
 */
#include "kcomp.h"
#include "kcomp_filesystem.h"
#include <errno.h>
#include <string.h>

/* 不透明绑定的内部表示。header 的 `opaque[4]` 至少这么大、对齐至少这么强
 * （下面的静态断言钉住；RV32 上内部表示更小，只用前缀）。 */
struct kcomp_filesystem_binding_internal {
    uint32_t mechanism; /* KCORE_ENDPOINT_MECHANISM_* */
    uint32_t reserved;  /* 保持 8 字节对齐，无语义 */
    uint64_t endpoint;  /* GATE：opaque EndpointId（call-gate handle） */
    const struct kcomp_filesystem_api *api; /* DIRECT：provider function table */
    void *ctx;                              /* DIRECT：provider opaque state */
};

_Static_assert(sizeof(struct kcomp_filesystem_binding_internal) <=
                   sizeof(struct kcomp_filesystem_binding),
               "kcomp_filesystem_binding too small for its internal representation");
_Static_assert(_Alignof(struct kcomp_filesystem_binding_internal) <=
                   _Alignof(struct kcomp_filesystem_binding),
               "kcomp_filesystem_binding alignment drift");

static struct kcomp_filesystem_binding_internal *binding_mut(
    struct kcomp_filesystem_binding *binding)
{
    return (struct kcomp_filesystem_binding_internal *)(void *)binding;
}

static const struct kcomp_filesystem_binding_internal *binding_ref(
    const struct kcomp_filesystem_binding *binding)
{
    return (const struct kcomp_filesystem_binding_internal *)(const void *)binding;
}

/* 内部失败（Core 回复违反契约 / 绑定未初始化）的传输状态：不猜测、不降级。 */
static struct kcomp_call_result error_result(int32_t transport)
{
    struct kcomp_call_result result;

    result.transport = transport;
    result.method = 0;
    return result;
}

static uint64_t read_le64(const uint8_t *bytes)
{
    uint64_t value = 0;

    for (size_t i = 0; i < KCOMP_FILESYSTEM_HANDLE_LEN; i++) {
        value |= ((uint64_t)bytes[i]) << (8u * (uint32_t)i);
    }
    return value;
}

static void write_le64(uint8_t *bytes, uint64_t value)
{
    for (size_t i = 0; i < KCOMP_FILESYSTEM_HANDLE_LEN; i++) {
        bytes[i] = KCOMP_FILESYSTEM_U64_BYTE(value, i);
    }
}

int32_t kcomp_filesystem_bind(uint64_t endpoint, uint64_t contract, uint64_t abi,
                              struct kcomp_filesystem_binding *out_binding)
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

    struct kcomp_filesystem_binding_internal *binding = binding_mut(out_binding);
    binding->reserved = 0;

    if (mechanism == KCORE_ENDPOINT_MECHANISM_DIRECT) {
        if (api_raw == 0) {
            /* Core 契约：Direct 必带非空 function table。 */
            return -EPROTO;
        }
        binding->mechanism = mechanism;
        binding->endpoint = 0;
        binding->api = (const struct kcomp_filesystem_api *)(uintptr_t)api_raw;
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

struct kcomp_call_result kcomp_filesystem_mount(const struct kcomp_filesystem_binding *binding)
{
    if (binding == NULL) {
        return error_result(-EINVAL);
    }
    const struct kcomp_filesystem_binding_internal *b = binding_ref(binding);
    struct kcomp_call_result result;

    if (b->mechanism == KCORE_ENDPOINT_MECHANISM_DIRECT) {
        if (b->api == NULL || b->api->mount == NULL) {
            return error_result(-EPROTO);
        }
        result.transport = 0;
        result.method = b->api->mount(b->ctx);
        return result;
    }

    if (b->mechanism == KCORE_ENDPOINT_MECHANISM_GATE) {
        result.method = 0;
        result.transport = kcore_endpoint_call(b->endpoint, KCOMP_FILESYSTEM_METHOD_MOUNT, NULL, 0,
                                               NULL, 0, NULL, 0, &result.method);
        return result;
    }

    return error_result(-EINVAL);
}

struct kcomp_call_result kcomp_filesystem_unmount(const struct kcomp_filesystem_binding *binding)
{
    if (binding == NULL) {
        return error_result(-EINVAL);
    }
    const struct kcomp_filesystem_binding_internal *b = binding_ref(binding);
    struct kcomp_call_result result;

    if (b->mechanism == KCORE_ENDPOINT_MECHANISM_DIRECT) {
        if (b->api == NULL || b->api->unmount == NULL) {
            return error_result(-EPROTO);
        }
        result.transport = 0;
        result.method = b->api->unmount(b->ctx);
        return result;
    }

    if (b->mechanism == KCORE_ENDPOINT_MECHANISM_GATE) {
        result.method = 0;
        result.transport = kcore_endpoint_call(b->endpoint, KCOMP_FILESYSTEM_METHOD_UNMOUNT, NULL,
                                               0, NULL, 0, NULL, 0, &result.method);
        return result;
    }

    return error_result(-EINVAL);
}

struct kcomp_call_result kcomp_filesystem_open(const struct kcomp_filesystem_binding *binding,
                                               const char *path, uint32_t flags,
                                               uint64_t *out_handle)
{
    if (binding == NULL || path == NULL || out_handle == NULL) {
        return error_result(-EINVAL);
    }
    *out_handle = 0;

    const struct kcomp_filesystem_binding_internal *b = binding_ref(binding);
    struct kcomp_call_result result;

    if (b->mechanism == KCORE_ENDPOINT_MECHANISM_DIRECT) {
        if (b->api == NULL || b->api->open == NULL) {
            return error_result(-EPROTO);
        }
        result.transport = 0;
        result.method = b->api->open(b->ctx, path, flags, out_handle);
        return result;
    }

    if (b->mechanism == KCORE_ENDPOINT_MECHANISM_GATE) {
        uint8_t args[KCOMP_FILESYSTEM_FLAGS_LEN];
        uint8_t reply[KCOMP_FILESYSTEM_HANDLE_LEN];

        size_t path_len = strlen(path) + 1; /* 含结尾 NUL（线格式要求） */
        if (path_len > KCOMP_FILESYSTEM_PATH_MAX) {
            return error_result(-EINVAL);
        }
        for (size_t i = 0; i < KCOMP_FILESYSTEM_FLAGS_LEN; i++) {
            args[i] = KCOMP_FILESYSTEM_U32_BYTE(flags, i);
        }
        result.method = 0;
        result.transport = kcore_endpoint_call(b->endpoint, KCOMP_FILESYSTEM_METHOD_OPEN, args,
                                               sizeof(args), (const uint8_t *)path, path_len,
                                               reply, sizeof(reply), &result.method);
        if (result.transport == 0 && result.method == 0) {
            *out_handle = read_le64(reply);
        }
        return result;
    }

    return error_result(-EINVAL);
}

struct kcomp_call_result kcomp_filesystem_close(const struct kcomp_filesystem_binding *binding,
                                                uint64_t handle)
{
    if (binding == NULL) {
        return error_result(-EINVAL);
    }
    const struct kcomp_filesystem_binding_internal *b = binding_ref(binding);
    struct kcomp_call_result result;

    if (b->mechanism == KCORE_ENDPOINT_MECHANISM_DIRECT) {
        if (b->api == NULL || b->api->close == NULL) {
            return error_result(-EPROTO);
        }
        result.transport = 0;
        result.method = b->api->close(b->ctx, handle);
        return result;
    }

    if (b->mechanism == KCORE_ENDPOINT_MECHANISM_GATE) {
        uint8_t args[KCOMP_FILESYSTEM_HANDLE_LEN];

        write_le64(args, handle);
        result.method = 0;
        result.transport = kcore_endpoint_call(b->endpoint, KCOMP_FILESYSTEM_METHOD_CLOSE, args,
                                               sizeof(args), NULL, 0, NULL, 0, &result.method);
        return result;
    }

    return error_result(-EINVAL);
}

struct kcomp_call_result kcomp_filesystem_read(const struct kcomp_filesystem_binding *binding,
                                               uint64_t handle, void *output, size_t output_len,
                                               size_t *out_read)
{
    if (binding == NULL || output == NULL || out_read == NULL) {
        return error_result(-EINVAL);
    }
    *out_read = 0;

    size_t capacity = output_len;
    uint8_t *data = output;

    const struct kcomp_filesystem_binding_internal *b = binding_ref(binding);
    struct kcomp_call_result result;

    if (b->mechanism == KCORE_ENDPOINT_MECHANISM_DIRECT) {
        size_t actual = 0;

        if (b->api == NULL || b->api->read == NULL) {
            return error_result(-EPROTO);
        }
        result.transport = 0;
        result.method = b->api->read(b->ctx, handle, data, capacity, &actual);
        if (result.method == 0) {
            if (actual > capacity) {
                /* provider 返回超过 buffer 的长度 = 契约违约（不截断）。 */
                return error_result(-EPROTO);
            }
            *out_read = actual;
        }
        return result;
    }

    if (b->mechanism == KCORE_ENDPOINT_MECHANISM_GATE) {
        uint8_t args[KCOMP_FILESYSTEM_HANDLE_LEN];
        uint8_t frame[KCOMP_FILESYSTEM_READ_HEADER_LEN + 512] = {0};
        capacity = capacity > 512 ? 512 : capacity;
        write_le64(args, handle);
        result.method = 0;
        result.transport = kcore_endpoint_call(b->endpoint, KCOMP_FILESYSTEM_METHOD_READ, args,
                                               sizeof(args), NULL, 0, frame,
                                               KCOMP_FILESYSTEM_READ_HEADER_LEN + capacity,
                                               &result.method);
        if (result.transport == 0 && result.method == 0) {
            uint64_t actual = read_le64(frame);
            if (actual > capacity) {
                return error_result(-EPROTO);
            }
            memcpy(data, frame + KCOMP_FILESYSTEM_READ_HEADER_LEN, (size_t)actual);
            *out_read = (size_t)actual;
        }
        return result;
    }

    return error_result(-EINVAL);
}

struct kcomp_call_result kcomp_filesystem_root(const struct kcomp_filesystem_binding *binding,
                                               uint64_t *out_node)
{
    if (binding == NULL || out_node == NULL)
        return error_result(-EINVAL);
    *out_node = 0;
    const struct kcomp_filesystem_binding_internal *b = binding_ref(binding);
    struct kcomp_call_result result = {0, 0};
    if (b->mechanism == KCORE_ENDPOINT_MECHANISM_DIRECT) {
        if (b->api == NULL || b->api->root == NULL)
            return error_result(-EPROTO);
        result.method = b->api->root(b->ctx, out_node);
    } else if (b->mechanism == KCORE_ENDPOINT_MECHANISM_GATE) {
        uint8_t reply[KCOMP_FILESYSTEM_HANDLE_LEN];
        result.transport = kcore_endpoint_call(b->endpoint, KCOMP_FILESYSTEM_METHOD_ROOT,
                                               NULL, 0, NULL, 0, reply, sizeof(reply), &result.method);
        if (result.transport == 0 && result.method == 0)
            *out_node = read_le64(reply);
    } else {
        return error_result(-EINVAL);
    }
    if (result.transport == 0 && result.method == 0 && *out_node == 0)
        return error_result(-EPROTO);
    return result;
}

struct kcomp_call_result kcomp_filesystem_lookup(const struct kcomp_filesystem_binding *binding,
                                                 uint64_t parent, const uint8_t *name,
                                                 size_t name_len, uint32_t encoding,
                                                 uint64_t *out_node)
{
    if (binding == NULL || name == NULL || out_node == NULL || name_len == 0 ||
        name_len > KCOMP_FILESYSTEM_NAME_MAX)
        return error_result(-EINVAL);
    *out_node = 0;
    const struct kcomp_filesystem_binding_internal *b = binding_ref(binding);
    struct kcomp_call_result result = {0, 0};
    if (b->mechanism == KCORE_ENDPOINT_MECHANISM_DIRECT) {
        if (b->api == NULL || b->api->lookup == NULL)
            return error_result(-EPROTO);
        result.method = b->api->lookup(b->ctx, parent, name, name_len, encoding, out_node);
    } else if (b->mechanism == KCORE_ENDPOINT_MECHANISM_GATE) {
        uint8_t args[KCOMP_FILESYSTEM_LOOKUP_ARGS_LEN];
        uint8_t reply[KCOMP_FILESYSTEM_HANDLE_LEN];
        write_le64(args, parent);
        for (size_t i = 0; i < KCOMP_FILESYSTEM_FLAGS_LEN; ++i)
            args[KCOMP_FILESYSTEM_HANDLE_LEN + i] = KCOMP_FILESYSTEM_U32_BYTE(encoding, i);
        result.transport = kcore_endpoint_call(b->endpoint, KCOMP_FILESYSTEM_METHOD_LOOKUP,
                                               args, sizeof(args), name, name_len,
                                               reply, sizeof(reply), &result.method);
        if (result.transport == 0 && result.method == 0)
            *out_node = read_le64(reply);
    } else {
        return error_result(-EINVAL);
    }
    if (result.transport == 0 && result.method == 0 && *out_node == 0)
        return error_result(-EPROTO);
    return result;
}

struct kcomp_call_result kcomp_filesystem_node_info(const struct kcomp_filesystem_binding *binding,
                                                    uint64_t node, uint32_t *out_kind)
{
    if (binding == NULL || out_kind == NULL)
        return error_result(-EINVAL);
    *out_kind = 0;
    const struct kcomp_filesystem_binding_internal *b = binding_ref(binding);
    struct kcomp_call_result result = {0, 0};
    if (b->mechanism == KCORE_ENDPOINT_MECHANISM_DIRECT) {
        if (b->api == NULL || b->api->node_info == NULL)
            return error_result(-EPROTO);
        result.method = b->api->node_info(b->ctx, node, out_kind);
    } else if (b->mechanism == KCORE_ENDPOINT_MECHANISM_GATE) {
        uint8_t args[KCOMP_FILESYSTEM_HANDLE_LEN];
        uint8_t reply[KCOMP_FILESYSTEM_FLAGS_LEN];
        write_le64(args, node);
        result.transport = kcore_endpoint_call(b->endpoint, KCOMP_FILESYSTEM_METHOD_NODE_INFO,
                                               args, sizeof(args), NULL, 0,
                                               reply, sizeof(reply), &result.method);
        if (result.transport == 0 && result.method == 0) {
            for (size_t i = 0; i < sizeof(reply); ++i)
                *out_kind |= (uint32_t)reply[i] << (8u * (uint32_t)i);
        }
    } else {
        return error_result(-EINVAL);
    }
    return result;
}
