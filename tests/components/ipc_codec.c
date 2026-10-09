/* Host harness for production C envelope; mock only the Core transport. */
#include "kcomp.h"
#include "kcomp_ipc.h"
#include <assert.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static size_t capacity;
static const char *reply_status;
static void hex(const uint8_t *bytes, size_t len) {
    for (size_t i = 0; i < len; ++i) printf("%02x", bytes[i]);
}
static size_t unhex(const char *text, uint8_t *bytes) {
    size_t n = strlen(text) / 2;
    assert(n <= 2048);
    for (size_t i = 0; i < n; ++i) {
        unsigned byte;
        assert(sscanf(text + i * 2, "%2x", &byte) == 1);
        bytes[i] = (uint8_t)byte;
    }
    return n;
}
int32_t kcore_ipc_submit(uint64_t ep, const uint8_t *bytes, size_t len, uint64_t *id) {
    assert(ep == 7);
    printf("request="); hex(bytes, len); puts("");
    if (!strcmp(reply_status, "transport")) return -EIO;
    capacity = kcomp_ipc_u32(bytes + 4);
    *id = 1;
    return 0;
}
int32_t kcore_ipc_collect(uint64_t id, uint8_t *bytes, size_t n, size_t *len, int32_t *completion) {
    assert(id == 1 && n >= capacity + 4);
    kcomp_ipc_put32(bytes, (uint32_t)strtol(reply_status, NULL, 10));
    memset(bytes + 4, 0xa5, capacity);
    *len = capacity + 4;
    *completion = 0;
    return 0;
}
int32_t kcore_ipc_wait(uint64_t ep, uint64_t id) { (void)ep; (void)id; abort(); }
int32_t kcore_ipc_cancel(uint64_t id) { (void)id; abort(); }
int main(int argc, char **argv) {
    uint8_t args[2048], input[2048], output[2048] = {0};
    assert(argc >= 3);
    if (!strcmp(argv[1], "decode")) {
        size_t len = unhex(argv[2], input);
        struct kcomp_ipc_request r;
        int32_t rc = kcomp_ipc_decode(input, len, &r);
        if (rc) printf("error=%d\n", rc);
        else {
            printf("method=%u output=%zu args=", r.method, r.output_len);
            hex(r.args, r.args_len); printf(" input="); hex(r.input, r.input_len); puts("");
        }
    } else {
        assert(argc == 7);
        size_t a = unhex(argv[3], args), b = unhex(argv[4], input);
        size_t n = (size_t)strtoul(argv[5], NULL, 10);
        assert(n <= sizeof(output));
        reply_status = argv[6];
        int32_t status = 0;
        int32_t rc = kcomp_ipc_invoke(7, (uint32_t)strtoul(argv[2], NULL, 10), args, a, input, b, output, n, &status);
        if (rc) printf("transport=%d\n", rc);
        else { printf("transport=0 method=%d output=", status); hex(output, n); puts(""); }
    }
    return 0;
}
