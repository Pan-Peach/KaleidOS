/* Actual generated clients/dispatchers; mock only Core transport. */
#include "kcomp.h"
#include "generated/block_wire.h"
#include "generated/echo_wire.h"
#include "methods_wire.h"
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
static uint8_t request[1024];
static size_t request_len, calls;
static const char *contract, *failure;
static void hex(const uint8_t *bytes, size_t len) { for (size_t i=0;i<len;++i) printf("%02x",bytes[i]); }
static size_t unhex(const char *text, uint8_t *bytes) {
    size_t n=strlen(text)/2; assert(n<=2048);
    for(size_t i=0;i<n;++i) { unsigned b; assert(sscanf(text+2*i,"%2x",&b)==1); bytes[i]=(uint8_t)b; } return n;
}
int32_t kcomp_block_wire_handle_capacity_sectors(void *ctx, struct kcomp_block_wire_capacity_sectors_reply *reply) {
    (void)ctx; ++calls; reply->sectors=UINT64_C(0x0102030405060708); return 0;
}
int32_t kcomp_block_wire_handle_read(void *ctx, uint64_t lba, uint8_t *output, size_t len) {
    (void)ctx; ++calls; if(lba==13) return -EIO; memset(output,(uint8_t)lba,len); return 0;
}
int32_t kcomp_block_wire_handle_write(void *ctx, uint64_t lba, const uint8_t *input, size_t len) {
    (void)ctx; (void)lba; (void)input; (void)len; ++calls; return 0;
}
int32_t kcomp_echo_wire_handle_echo(void *ctx, const uint8_t *input, size_t n, uint8_t *output, size_t len) {
    (void)ctx; assert(n==len); ++calls; if(n) memcpy(output,input,n); return 0;
}
int32_t kcomp_methods_wire_handle_numbers(void *ctx, uint8_t a_u8, uint16_t a_u16, uint32_t a_u32, uint64_t a_u64,
    int8_t a_i8, int16_t a_i16, int32_t a_i32, int64_t a_i64, struct kcomp_methods_wire_numbers_reply *reply) {
    (void)ctx; ++calls;
    *reply=(struct kcomp_methods_wire_numbers_reply){a_u8,a_u16,a_u32,a_u64,a_i8,a_i16,a_i32,a_i64}; return 0;
}
int32_t kcomp_methods_wire_handle_flush(void *ctx) { (void)ctx; ++calls; return 0; }
static int32_t dispatch(const uint8_t *bytes, size_t n, uint8_t *output, size_t len) {
    struct kcomp_ipc_request r; int32_t status=kcomp_ipc_decode(bytes,n,&r); if(status) return status;
    if(!strcmp(contract,"block")) return kcomp_block_wire_dispatch(NULL,&r,output,len);
    if(!strcmp(contract,"echo")) return kcomp_echo_wire_dispatch(NULL,&r,output,len);
    return kcomp_methods_wire_dispatch(NULL,&r,output,len);
}
int32_t kcore_ipc_submit(uint64_t endpoint, const uint8_t *bytes, size_t len, uint64_t *id) {
    assert(endpoint==7); printf("request="); hex(bytes,len); puts("");
    if(failure && !strcmp(failure,"transport")) return -EACCES;
    assert(len<=sizeof(request)); memcpy(request,bytes,len); request_len=len; *id=1; return 0;
}
int32_t kcore_ipc_collect(uint64_t id, uint8_t *bytes, size_t capacity, size_t *len, int32_t *completion) {
    assert(id==1); size_t n=kcomp_ipc_u32(request+4); assert(capacity>=n+4); memset(bytes,0,n+4);
    int32_t status=dispatch(request,request_len,bytes+4,n); kcomp_ipc_put32(bytes,(uint32_t)status);
    printf("reply="); hex(bytes,n+4); puts(""); *len=n+4; *completion=0; return 0;
}
int32_t kcore_ipc_wait(uint64_t ep,uint64_t id) { (void)ep; (void)id; abort(); }
int32_t kcore_ipc_cancel(uint64_t id) { (void)id; abort(); }
int main(int argc,char **argv) {
    assert(argc>=5); contract=argv[2]; failure=argc>5?argv[5]:NULL;
    uint8_t input[2048],output[2048]={0}; size_t n=(size_t)strtoul(argv[4],NULL,10); assert(n<=sizeof(output));
    if(!strcmp(argv[1],"dispatch")) {
        int32_t status=dispatch(input,unhex(argv[3],input),output,n);
        printf("status=%d calls=%zu output=",status,calls); hex(output,n); puts(""); return 0;
    }
    int32_t status=0,transport;
    if(!strcmp(contract,"echo")) transport=kcomp_echo_wire_echo(7,input,unhex(argv[3],input),output,n,&status);
    else if(!strcmp(contract,"block")) {
        if(!strcmp(argv[3],"capacity")) {
            struct kcomp_block_wire_capacity_sectors_reply r;
            transport=kcomp_block_wire_capacity_sectors(7,&r,&status);
            if(!transport && !status) assert(r.sectors==UINT64_C(0x0102030405060708));
        } else if(!strcmp(argv[3],"write")) {
            memset(input,0x81,n); transport=kcomp_block_wire_write(7,7,input,n,&status);
        } else transport=kcomp_block_wire_read(7,strtoull(argv[3],NULL,10),output,n,&status);
    } else if(!strcmp(contract,"numbers")) {
        struct kcomp_methods_wire_numbers_reply r;
        transport=kcomp_methods_wire_numbers(7,0xff,0xabcd,0x89abcdef,UINT64_C(0xfedcba9876543210),
            INT8_MIN,INT16_MIN,INT32_MIN,INT64_MIN,&r,&status);
        if(!transport && !status) assert(r.b_i64==INT64_MIN);
    } else transport=kcomp_methods_wire_flush(7,&status);
    if(transport) printf("transport=%d calls=%zu\n",transport,calls);
    else printf("transport=0 method=%d calls=%zu\n",status,calls);
    return 0;
}
