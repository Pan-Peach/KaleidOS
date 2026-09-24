/* littlefs_service.c —— littlefs 的 **Gate 侧入口**（image 级统一 dispatcher）。
 *
 * Core 的 `kcore_endpoint_call` 经本组件的 `kcomp_service_dispatch` 分派：`port`
 * 选中 provider 的哪个 endpoint（本组件只有一个 filesystem endpoint），`method`
 * 落到 `abi/filesystem.toml` 定义的扁平编码。业务后端是 littlefs.c 里的
 * `littlefs_mount` / `littlefs_open` / `littlefs_read` …——与 Direct 的 function
 * table 共用同一份实现（业务代码不感知部署）。
 *
 * # 严格校验（镜像 SDK Rust dispatch 的严格度）
 *
 * 每一个 method 的 args / input / output 长度与形状都必须精确符合 schema；畸形帧
 * 一律 `-EINVAL` 且业务后端**不被调用**。`open` 的路径必须 NUL 结尾、结尾 NUL 是
 * 唯一 NUL、长度含 NUL 不超过 `KCOMP_FILESYSTEM_PATH_MAX`，并且**先拷进本文件的
 * scratch**——绝不保留、绝不改写 caller 内存。`read` 的 output 必须放得下 8 字节
 * LE 长度头，数据区容量 = `output_len - 8`。
 */
#include "kcomp.h"
#include "littlefs_internal.h"
#include <errno.h>
#include <string.h>

static uint32_t read_le32(const uint8_t *bytes)
{
    uint32_t value = 0;

    for (size_t i = 0; i < KCOMP_FILESYSTEM_FLAGS_LEN; i++) {
        value |= ((uint32_t)bytes[i]) << (8u * (uint32_t)i);
    }
    return value;
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

/* Gate 机制证据：每次经 Core call gate 的调用都打一行。QEMU runner 用它做差分
 * 断言（唯一一条应是组合器的探针；稳态业务调用**不应**伴随本行）。 */
static void log_gate_dispatch(uint32_t method)
{
    static const char prefix[] = "[littlefs] gate dispatch method=";
    char line[sizeof(prefix) + 10];
    char digits[10];
    size_t len = sizeof(prefix) - 1;
    size_t count = 0;

    memcpy(line, prefix, len);
    do {
        digits[count++] = (char)('0' + (method % 10u));
        method /= 10u;
    } while (method != 0);
    while (count > 0) {
        line[len++] = digits[--count];
    }
    kcore_log_line((const uint8_t *)line, len);
}

/* 无参方法（mount / unmount）：args / input / output 都必须为空。 */
static int empty_frame(const struct kcomp_call_frame *frame)
{
    return frame->args_len == 0 && frame->input_len == 0 && frame->output_len == 0;
}

static int32_t dispatch_open(struct littlefs_state *state, const struct kcomp_call_frame *frame)
{
    if (frame->args_len != KCOMP_FILESYSTEM_FLAGS_LEN) {
        return -EINVAL;
    }
    if (frame->input == NULL || frame->input_len == 0 ||
        frame->input_len > KCOMP_FILESYSTEM_PATH_MAX) {
        return -EINVAL;
    }
    if (frame->input[frame->input_len - 1] != 0) {
        return -EINVAL;
    }
    /* 结尾 NUL 必须是**唯一** NUL（与 Rust dispatch 的 CStr::from_bytes_with_nul
     * 同一严格度：内部 NUL 会静默截断路径，拒绝而不是猜）。 */
    for (size_t i = 0; i + 1 < frame->input_len; i++) {
        if (frame->input[i] == 0) {
            return -EINVAL;
        }
    }
    if (frame->output == NULL || frame->output_len != KCOMP_FILESYSTEM_HANDLE_LEN) {
        return -EINVAL;
    }

    /* 拷进自己的 scratch：绝不保留 / 改写 caller 内存。 */
    char scratch[KCOMP_FILESYSTEM_PATH_MAX];
    memcpy(scratch, frame->input, frame->input_len);

    uint64_t handle = 0;
    int32_t result = littlefs_open(state, scratch, read_le32(frame->args), &handle);
    if (result != 0) {
        return result;
    }
    write_le64(frame->output, handle);
    return 0;
}

static int32_t dispatch_close(struct littlefs_state *state, const struct kcomp_call_frame *frame)
{
    if (frame->args_len != KCOMP_FILESYSTEM_HANDLE_LEN || frame->input_len != 0 ||
        frame->output_len != 0) {
        return -EINVAL;
    }
    return littlefs_close(state, read_le64(frame->args));
}

static int32_t dispatch_read(struct littlefs_state *state, const struct kcomp_call_frame *frame)
{
    if (frame->args_len != KCOMP_FILESYSTEM_HANDLE_LEN || frame->input_len != 0) {
        return -EINVAL;
    }
    if (frame->output == NULL || frame->output_len < KCOMP_FILESYSTEM_READ_HEADER_LEN) {
        return -EINVAL;
    }

    size_t capacity = frame->output_len - KCOMP_FILESYSTEM_READ_HEADER_LEN;
    size_t actual = 0;
    int32_t result = littlefs_read(state, read_le64(frame->args),
                                   frame->output + KCOMP_FILESYSTEM_READ_HEADER_LEN, capacity,
                                   &actual);
    if (result != 0) {
        return result;
    }
    if (actual > capacity) {
        /* provider 返回超过 buffer 的长度 = 契约违约（不截断、不 UB 兜底）。 */
        return -EIO;
    }
    write_le64(frame->output, (uint64_t)actual);
    return 0;
}

int32_t kcomp_service_dispatch(void *instance_state, uint32_t port, uint32_t method,
                               const struct kcomp_call_frame *frame)
{
    if (instance_state == NULL) {
        return -EINVAL;
    }
    if (port != LITTLEFS_PORT) {
        /* 本 image 只发布 filesystem endpoint；其它 port 是能力缺失。 */
        return -ENOSYS;
    }

    log_gate_dispatch(method);

    if (frame == NULL) {
        return -EINVAL;
    }
    struct littlefs_state *state = instance_state;

    switch (method) {
    case KCOMP_FILESYSTEM_METHOD_MOUNT:
        return empty_frame(frame) ? littlefs_mount(state) : -EINVAL;

    case KCOMP_FILESYSTEM_METHOD_UNMOUNT:
        return empty_frame(frame) ? littlefs_unmount(state) : -EINVAL;

    case KCOMP_FILESYSTEM_METHOD_OPEN:
        return dispatch_open(state, frame);

    case KCOMP_FILESYSTEM_METHOD_CLOSE:
        return dispatch_close(state, frame);

    case KCOMP_FILESYSTEM_METHOD_READ:
        return dispatch_read(state, frame);

    default:
        /* 能力缺失（不是畸形帧）：与 Core 对"没有 dispatcher"的档位一致。 */
        return -ENOSYS;
    }
}
