/* Actual C IPC Block facade: split requests, overflow and partial completion. */
#include "kcomp.h"
#include "kcomp_ipc.h"
#include <assert.h>
#include <errno.h>
#include <string.h>
static unsigned calls, fail_at;
static uint32_t mechanism=KCORE_ENDPOINT_MECHANISM_IPC;
static uint64_t seen[8];
int32_t kcore_endpoint_bind(uint64_t id, uint64_t contract, uint64_t abi,
    uint32_t *mode, size_t *api, size_t *ctx) {
    assert(id==7 && contract==KCOMP_BLOCK_DEVICE_CONTRACT && abi==KCOMP_BLOCK_DEVICE_ABI);
    *mode=mechanism; *api=0; *ctx=0; return 0;
}
int32_t kcomp_ipc_invoke(uint64_t endpoint, uint32_t method, const void *args,
    size_t args_len, const void *input, size_t input_len, void *output,
    size_t output_len, int32_t *status) {
    assert(endpoint==7);
    *status=0;
    if(method==KCOMP_BLOCK_METHOD_CAPACITY) {
        assert(args_len==0 && input_len==0 && output_len==8);
        memset(output,0,8); ((uint8_t *)output)[0]=64; return 0;
    }
    assert(args_len==8 && calls<8);
    const uint8_t *bytes=args; uint64_t lba=0;
    for(size_t i=0;i<8;++i) lba|=(uint64_t)bytes[i]<<(8*i);
    seen[calls++]=lba;
    if(calls==fail_at) { *status=-EIO; return 0; }
    if(method==KCOMP_BLOCK_METHOD_READ) {
        assert(input_len==0 && output_len==512); memset(output,(uint8_t)lba,512);
    } else {
        assert(method==KCOMP_BLOCK_METHOD_WRITE && input_len==512 && output_len==0);
        assert(((const uint8_t *)input)[0]==(uint8_t)lba);
    }
    return 0;
}
int main(void) {
    struct kcomp_block_binding binding;
    assert(kcomp_block_bind(7,KCOMP_BLOCK_DEVICE_CONTRACT,KCOMP_BLOCK_DEVICE_ABI,&binding)==0);
    uint64_t sectors=0; struct kcomp_call_result r=kcomp_block_capacity(&binding,&sectors);
    assert(!r.transport && !r.method && sectors==64);
    uint8_t buffer[1024]; memset(buffer,0xa5,sizeof(buffer));
    r=kcomp_block_read(&binding,7,buffer,sizeof(buffer));
    assert(!r.transport && !r.method && calls==2 && seen[0]==7 && seen[1]==8);
    for(size_t i=0;i<sizeof(buffer);++i) assert(buffer[i]==(i<512?7:8));
    calls=0; r=kcomp_block_write(&binding,7,buffer,sizeof(buffer));
    assert(!r.transport && !r.method && calls==2 && seen[1]==8);
    calls=0;
    r=kcomp_block_read(&binding,UINT64_MAX,buffer,sizeof(buffer));
    assert(!r.transport && r.method==-EOVERFLOW && !calls);
    r=kcomp_block_write(&binding,0,buffer,513); assert(r.transport==-EINVAL && !calls);
    r=kcomp_block_read(&binding,0,buffer,0); assert(r.transport==-EINVAL && !calls);
    r=kcomp_block_read(&binding,0,NULL,512); assert(r.transport==-EINVAL && !calls);
    memset(buffer,0xa5,sizeof(buffer)); fail_at=2;
    r=kcomp_block_read(&binding,7,buffer,sizeof(buffer));
    assert(!r.transport && r.method==-EIO && calls==2);
    for(size_t i=0;i<sizeof(buffer);++i) assert(buffer[i]==(i<512?7:0xa5));
    calls=0; fail_at=0;
    r=kcomp_block_read(&binding,UINT64_MAX,buffer,512);
    assert(!r.transport && !r.method && calls==1 && seen[0]==UINT64_MAX);
    mechanism=KCORE_ENDPOINT_MECHANISM_DIRECT;
    assert(kcomp_block_bind(7,KCOMP_BLOCK_DEVICE_CONTRACT,KCOMP_BLOCK_DEVICE_ABI,&binding)==-ENOTSUP);
    r=kcomp_block_read(&binding,0,buffer,512); assert(r.transport==-EINVAL && calls==1);
    return 0;
}
