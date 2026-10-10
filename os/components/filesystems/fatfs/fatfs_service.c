/* Local business handlers; generated filesystem_wire owns all protocol shapes. */
#include "fatfs_internal.h"
#include <errno.h>
#include <string.h>

int32_t kcomp_filesystem_wire_handle_mount(void *ctx) { return fatfs_mount(ctx); }
int32_t kcomp_filesystem_wire_handle_unmount(void *ctx) { return fatfs_unmount(ctx); }
int32_t kcomp_filesystem_wire_handle_close(void *ctx, uint64_t handle) { return fatfs_close(ctx, handle); }
int32_t kcomp_filesystem_wire_handle_root(void *ctx, struct kcomp_filesystem_wire_root_reply *reply) {
    return fatfs_root(ctx, &reply->node);
}
int32_t kcomp_filesystem_wire_handle_lookup(void *ctx, uint64_t parent, uint32_t encoding,
    const uint8_t *name, size_t name_len, struct kcomp_filesystem_wire_lookup_reply *reply) {
    return fatfs_lookup(ctx, parent, name, name_len, encoding, &reply->node);
}
int32_t kcomp_filesystem_wire_handle_node_info(void *ctx, uint64_t node, struct kcomp_filesystem_wire_node_info_reply *reply) {
    return fatfs_node_info(ctx, node, &reply->kind);
}
int32_t kcomp_filesystem_wire_handle_open_node(void *ctx, uint64_t node, struct kcomp_filesystem_wire_open_node_reply *reply) {
    return fatfs_open_node(ctx, node, &reply->handle);
}
int32_t kcomp_filesystem_wire_handle_open(void *ctx, uint32_t flags, const uint8_t *input, size_t input_len,
    struct kcomp_filesystem_wire_open_reply *reply) {
    if (!input_len || input_len > KCOMP_FILESYSTEM_PATH_MAX || input[input_len - 1]) return -EINVAL;
    for (size_t i = 0; i + 1 < input_len; ++i) if (!input[i]) return -EINVAL;
    char path[KCOMP_FILESYSTEM_PATH_MAX];
    memcpy(path, input, input_len);
    return fatfs_open(ctx, path, flags, &reply->handle);
}
int32_t kcomp_filesystem_wire_handle_read(void *ctx, uint64_t handle, uint8_t *output, size_t len,
    struct kcomp_filesystem_wire_read_reply *reply) {
    size_t actual = 0;
    int32_t rc = fatfs_read(ctx, handle, output, len, &actual);
    if (!rc && actual > len) return -EIO;
    reply->actual = actual;
    return rc;
}
int32_t kcomp_filesystem_wire_handle_read_at(void *ctx, uint64_t handle, uint64_t offset,
    uint8_t *output, size_t len, struct kcomp_filesystem_wire_read_at_reply *reply) {
    size_t actual = 0;
    int32_t rc = fatfs_read_at(ctx, handle, offset, output, len, &actual);
    if (!rc && actual > len) return -EIO;
    reply->actual = actual;
    return rc;
}
int32_t kcomp_filesystem_wire_handle_shutdown(void *ctx) {
    (void)ctx;
    /* Only the Server's verified control-consumer branch may shut down. */
    return -EACCES;
}
