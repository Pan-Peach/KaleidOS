/* Local C business handlers. Generated dispatch owns all protocol shapes. */
#include "littlefs_internal.h"
#include <errno.h>
#include <string.h>
int32_t kcomp_filesystem_wire_handle_mount(void *ctx) { return littlefs_mount(ctx); }
int32_t kcomp_filesystem_wire_handle_unmount(void *ctx) { return littlefs_unmount(ctx); }
int32_t kcomp_filesystem_wire_handle_close(void *ctx, uint64_t handle) { return littlefs_close(ctx, handle); }
int32_t kcomp_filesystem_wire_handle_root(void *ctx, struct kcomp_filesystem_wire_root_reply *reply) {
    (void)ctx; (void)reply;
    return -ENOTSUP;
}
int32_t kcomp_filesystem_wire_handle_lookup(void *ctx, uint64_t parent, uint32_t encoding,
    const uint8_t *name, size_t name_len, struct kcomp_filesystem_wire_lookup_reply *reply) {
    (void)ctx; (void)parent; (void)encoding; (void)name; (void)name_len; (void)reply;
    return -ENOTSUP;
}
int32_t kcomp_filesystem_wire_handle_node_info(void *ctx, uint64_t node, struct kcomp_filesystem_wire_node_info_reply *reply) {
    (void)ctx; (void)node; (void)reply;
    return -ENOTSUP;
}
int32_t kcomp_filesystem_wire_handle_open_node(void *ctx, uint64_t node, struct kcomp_filesystem_wire_open_node_reply *reply) {
    (void)ctx; (void)node; (void)reply;
    return -ENOTSUP;
}
int32_t kcomp_filesystem_wire_handle_open(void *ctx, uint32_t flags, const uint8_t *input, size_t input_len,
    struct kcomp_filesystem_wire_open_reply *reply) {
    if (!input_len || input_len > KCOMP_FILESYSTEM_PATH_MAX || input[input_len - 1]) return -EINVAL;
    for (size_t i = 0; i + 1 < input_len; ++i) if (!input[i]) return -EINVAL;
    char path[KCOMP_FILESYSTEM_PATH_MAX];
    memcpy(path, input, input_len);
    return littlefs_open(ctx, path, flags, &reply->handle);
}
int32_t kcomp_filesystem_wire_handle_read(void *ctx, uint64_t handle, uint8_t *output, size_t len,
    struct kcomp_filesystem_wire_read_reply *reply) {
    size_t actual = 0;
    int32_t rc = littlefs_read(ctx, handle, output, len, &actual);
    if (!rc && actual > len) return -EIO;
    reply->actual = actual;
    return rc;
}
int32_t kcomp_filesystem_wire_handle_read_at(void *ctx, uint64_t handle, uint64_t offset,
    uint8_t *output, size_t len, struct kcomp_filesystem_wire_read_at_reply *reply) {
    (void)ctx; (void)handle; (void)offset; (void)output; (void)len; (void)reply;
    return -ENOTSUP;
}
int32_t kcomp_filesystem_wire_handle_shutdown(void *ctx) {
    (void)ctx;
    /* Only the Server's verified control-consumer branch may shut down. */
    return -EACCES;
}

int32_t kcomp_filesystem_wire_handle_node_details(void *ctx, uint64_t node, uint8_t *output,
    size_t len, struct kcomp_filesystem_wire_node_details_reply *reply) {
    (void)ctx; (void)node; (void)output; (void)len; (void)reply;
    return -ENOTSUP;
}
