/* Typed IPC facade. Generated clients own methods, shapes and LE codecs. */
#include "kcomp.h"
#include "generated/filesystem_wire.h"
#include <errno.h>
#include <string.h>
static struct kcomp_call_result error(int32_t rc) { return (struct kcomp_call_result){rc,0}; }
static uint64_t endpoint(const struct kcomp_filesystem_binding *binding) {
    return binding ? binding->opaque[0] : 0;
}
int32_t kcomp_filesystem_bind(uint64_t id, uint64_t contract, uint64_t abi,
                             struct kcomp_filesystem_binding *binding) {
    if (!binding) return -EINVAL;
    memset(binding,0,sizeof(*binding));
    if (contract != KCOMP_FILESYSTEM_CONTRACT || abi != KCOMP_FILESYSTEM_ABI) return -EINVAL;
    uint32_t mechanism=0; size_t api=0,ctx=0;
    int32_t rc = kcore_endpoint_bind(id,contract,abi,&mechanism,&api,&ctx);
    if (!rc && (mechanism!=KCORE_ENDPOINT_MECHANISM_IPC || api || ctx)) rc=-ENOTSUP;
    if (!rc) binding->opaque[0]=id;
    return rc;
}
struct kcomp_call_result kcomp_filesystem_mount(const struct kcomp_filesystem_binding *b) {
    if (!endpoint(b)) return error(-EINVAL);
    struct kcomp_call_result r={0,0};
    r.transport=kcomp_filesystem_wire_mount(endpoint(b), &r.method);
    return r;
}
struct kcomp_call_result kcomp_filesystem_unmount(const struct kcomp_filesystem_binding *b) {
    if (!endpoint(b)) return error(-EINVAL);
    struct kcomp_call_result r={0,0};
    r.transport=kcomp_filesystem_wire_unmount(endpoint(b), &r.method);
    return r;
}
struct kcomp_call_result kcomp_filesystem_shutdown(const struct kcomp_filesystem_binding *b) {
    if (!endpoint(b)) return error(-EINVAL);
    struct kcomp_call_result r={0,0};
    r.transport=kcomp_filesystem_wire_shutdown(endpoint(b), &r.method);
    return r;
}
struct kcomp_call_result kcomp_filesystem_open(const struct kcomp_filesystem_binding *b,
    const char *path, uint32_t flags, uint64_t *out) {
    if (!endpoint(b) || !path || !out) return error(-EINVAL);
    *out=0;
    size_t len=0; while (len<KCOMP_FILESYSTEM_PATH_MAX && path[len]) ++len;
    if (len==KCOMP_FILESYSTEM_PATH_MAX) return error(-EINVAL);
    struct kcomp_call_result r={0,0}; struct kcomp_filesystem_wire_open_reply reply;
    r.transport=kcomp_filesystem_wire_open(endpoint(b), flags, (const uint8_t *)path, len+1, &reply, &r.method);
    if (!r.transport && !r.method) { if (!reply.handle) return error(-EPROTO); *out=reply.handle; }
    return r;
}
struct kcomp_call_result kcomp_filesystem_close(const struct kcomp_filesystem_binding *b, uint64_t handle) {
    if (!endpoint(b)) return error(-EINVAL);
    struct kcomp_call_result r={0,0}; r.transport=kcomp_filesystem_wire_close(endpoint(b), handle, &r.method); return r;
}
struct kcomp_call_result kcomp_filesystem_read(const struct kcomp_filesystem_binding *b,
    uint64_t handle, void *output, size_t capacity, size_t *out) {
    if (!endpoint(b) || (!output && capacity) || !out) return error(-EINVAL);
    *out=0;
    uint8_t scratch[512]; size_t len=capacity<sizeof(scratch)?capacity:sizeof(scratch);
    struct kcomp_call_result r={0,0}; struct kcomp_filesystem_wire_read_reply reply;
    r.transport=kcomp_filesystem_wire_read(endpoint(b), handle, scratch, len, &reply, &r.method);
    if (!r.transport && !r.method) {
        if (reply.actual>len) return error(-EPROTO);
        if (reply.actual) memcpy(output,scratch,(size_t)reply.actual);
        *out=(size_t)reply.actual;
    }
    return r;
}
struct kcomp_call_result kcomp_filesystem_root(const struct kcomp_filesystem_binding *b, uint64_t *out) {
    if (!endpoint(b) || !out) return error(-EINVAL);
    *out=0;
    struct kcomp_call_result r={0,0}; struct kcomp_filesystem_wire_root_reply reply;
    r.transport=kcomp_filesystem_wire_root(endpoint(b), &reply, &r.method);
    if (!r.transport && !r.method) { if (!reply.node) return error(-EPROTO); *out=reply.node; }
    return r;
}
struct kcomp_call_result kcomp_filesystem_lookup(const struct kcomp_filesystem_binding *b,
    uint64_t parent, const uint8_t *name, size_t len, uint32_t encoding, uint64_t *out) {
    if (!endpoint(b) || !name || !out || !len || len>KCOMP_FILESYSTEM_NAME_MAX) return error(-EINVAL);
    *out=0;
    struct kcomp_call_result r={0,0}; struct kcomp_filesystem_wire_lookup_reply reply;
    r.transport=kcomp_filesystem_wire_lookup(endpoint(b), parent, encoding, name, len, &reply, &r.method);
    if (!r.transport && !r.method) { if (!reply.node) return error(-EPROTO); *out=reply.node; }
    return r;
}
struct kcomp_call_result kcomp_filesystem_node_info(const struct kcomp_filesystem_binding *b, uint64_t node, uint32_t *out) {
    if (!endpoint(b) || !out) return error(-EINVAL);
    *out=0;
    struct kcomp_call_result r={0,0}; struct kcomp_filesystem_wire_node_info_reply reply;
    r.transport=kcomp_filesystem_wire_node_info(endpoint(b), node, &reply, &r.method);
    if (!r.transport && !r.method) *out=reply.kind;
    return r;
}
