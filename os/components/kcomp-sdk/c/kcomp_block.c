/* Typed IPC facade; generated clients own codecs and method dispatch. */
#include "kcomp.h"
#include "generated/block_wire.h"
#include <errno.h>
#include <string.h>
static struct kcomp_call_result error(int32_t rc) { return (struct kcomp_call_result){rc,0}; }
static uint64_t endpoint(const struct kcomp_block_binding *binding) { return binding?binding->opaque[0]:0; }
int32_t kcomp_block_bind(uint64_t id, uint64_t contract, uint64_t abi, struct kcomp_block_binding *b) {
    if (!b) return -EINVAL;
    memset(b,0,sizeof(*b));
    if (contract!=KCOMP_BLOCK_DEVICE_CONTRACT || abi!=KCOMP_BLOCK_DEVICE_ABI) return -EINVAL;
    uint32_t mechanism=0; size_t api=0,ctx=0;
    int32_t rc=kcore_endpoint_bind(id,contract,abi,&mechanism,&api,&ctx);
    if (!rc && (mechanism!=KCORE_ENDPOINT_MECHANISM_IPC || api || ctx)) rc=-ENOTSUP;
    if (!rc) b->opaque[0]=id;
    return rc;
}
struct kcomp_call_result kcomp_block_read(const struct kcomp_block_binding *b,
    uint64_t lba, void *output, size_t len) {
    if (!endpoint(b) || !output || !len || len%512) return error(-EINVAL);
    size_t sectors=len/512;
    if (lba>UINT64_MAX-(sectors-1)) return (struct kcomp_call_result){0,-EOVERFLOW};
    for (size_t i=0; i<sectors; ++i) {
        struct kcomp_call_result r={0,0};
        r.transport=kcomp_block_wire_read(endpoint(b),lba+i,(uint8_t *)output+i*512,512,&r.method);
        if(r.transport || r.method) return r;
    }
    return (struct kcomp_call_result){0,0};
}
struct kcomp_call_result kcomp_block_write(const struct kcomp_block_binding *b,
    uint64_t lba, const void *input, size_t len) {
    if (!endpoint(b) || !input || !len || len%512) return error(-EINVAL);
    size_t sectors=len/512;
    if (lba>UINT64_MAX-(sectors-1)) return (struct kcomp_call_result){0,-EOVERFLOW};
    for (size_t i=0; i<sectors; ++i) {
        struct kcomp_call_result r={0,0};
        r.transport=kcomp_block_wire_write(endpoint(b),lba+i,(const uint8_t *)input+i*512,512,&r.method);
        if(r.transport || r.method) return r;
    }
    return (struct kcomp_call_result){0,0};
}
struct kcomp_call_result kcomp_block_capacity(const struct kcomp_block_binding *b, uint64_t *out) {
    if (!endpoint(b) || !out) return error(-EINVAL);
    *out=0;
    struct kcomp_call_result r={0,0}; struct kcomp_block_wire_capacity_sectors_reply reply;
    r.transport=kcomp_block_wire_capacity_sectors(endpoint(b),&reply,&r.method);
    if (!r.transport && !r.method) *out=reply.sectors;
    return r;
}
