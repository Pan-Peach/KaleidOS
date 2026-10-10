/* IPC facade bounds/short-read regression; fake only the message transport. */
#include <assert.h>
#include <string.h>
#include "kcomp.h"
#include "kcomp_ipc.h"
#include <errno.h>
static size_t seen;
static int invalid;
int32_t kcore_endpoint_bind(uint64_t id, uint64_t contract, uint64_t abi,
    uint32_t *mechanism, size_t *api, size_t *ctx) {
    assert(id==1 && contract==KCOMP_FILESYSTEM_CONTRACT && abi==KCOMP_FILESYSTEM_ABI);
    *mechanism=KCORE_ENDPOINT_MECHANISM_IPC; *api=0; *ctx=0; return 0;
}
int32_t kcomp_ipc_invoke(uint64_t endpoint, uint32_t method, const void *args_raw,
    size_t args_len, const void *input, size_t input_len, void *output_raw,
    size_t output_len, int32_t *status) {
    const uint8_t *args=args_raw; uint8_t *output=output_raw;
    (void)input;
    assert(endpoint==1 && method==KCOMP_FILESYSTEM_METHOD_READ);
    assert(args_len==8 && args[0]==7 && input_len==0 && output_len>=8);
    seen=output_len;
    memset(output,0,output_len);
    size_t actual=output_len-8; if(actual>3) actual=3;
    output[0]=invalid?255:(uint8_t)actual;
    memcpy(output+8,"ABC",actual); *status=0; return 0;
}
int main(void) {
    struct kcomp_filesystem_binding binding;
    assert(kcomp_filesystem_bind(1,KCOMP_FILESYSTEM_CONTRACT,KCOMP_FILESYSTEM_ABI,&binding)==0);
    uint8_t data[1024]; size_t actual;
    memset(data,0xa5,sizeof(data));
    struct kcomp_call_result r=kcomp_filesystem_read(&binding,7,data,1,&actual);
    assert(!r.transport && !r.method && actual==1 && data[0]=='A' && data[1]==0xa5);
    r=kcomp_filesystem_read(&binding,7,data,sizeof(data),&actual);
    assert(!r.transport && !r.method && actual==3 && !memcmp(data,"ABC",3) && seen==520);
    r=kcomp_filesystem_read(&binding,7,NULL,0,&actual);
    assert(!r.transport && !r.method && actual==0);
    invalid=1; uint8_t unchanged=0xa5; actual=42;
    r=kcomp_filesystem_read(&binding,7,&unchanged,1,&actual);
    assert(r.transport==-EPROTO && unchanged==0xa5 && actual==0);
    return 0;
}
