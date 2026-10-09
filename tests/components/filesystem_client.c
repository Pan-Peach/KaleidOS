#include <assert.h>
#include <string.h>
#include "kcomp.h"
#include <errno.h>
static uint32_t mechanism;
static size_t seen;
static int invalid;
static int32_t read_data(void *ctx, uint64_t handle, uint8_t *buf, size_t len, size_t *out)
{
    (void)ctx; assert(handle == 7);
    seen = len;
    *out = len > 3 ? 3 : len;
    memcpy(buf, "ABC", *out);
    return 0;
}
static const struct kcomp_filesystem_api api = { .read = read_data };
int32_t kcore_endpoint_bind(uint64_t endpoint, uint64_t contract, uint64_t abi,
                            uint32_t *out_mechanism, size_t *out_api, size_t *out_ctx)
{
    (void)endpoint; (void)contract; (void)abi;
    *out_mechanism = mechanism;
    *out_api = (size_t)&api; *out_ctx = 0;
    return 0;
}
int32_t kcore_endpoint_call(uint64_t endpoint, uint32_t method,
    const uint8_t *args, size_t args_len, const uint8_t *input, size_t input_len,
    uint8_t *output, size_t output_len, int32_t *out_method)
{
    (void)endpoint; (void)input;
    assert(method == KCOMP_FILESYSTEM_METHOD_READ && args_len == 8 && args[0] == 7);
    assert(input_len == 0 && output_len >= 8);
    seen = output_len;
    memset(output, 0, output_len);
    size_t actual = output_len - 8;
    if (actual > 3) actual = 3;
    output[0] = invalid ? 255 : actual;
    memcpy(output + 8, "ABC", actual);
    *out_method = 0;
    return 0;
}
int main(void)
{
    struct kcomp_filesystem_binding binding;
    for (unsigned int mode = 0; mode < 2; mode++) {
        mechanism = mode == 0 ? KCORE_ENDPOINT_MECHANISM_DIRECT : KCORE_ENDPOINT_MECHANISM_GATE;
        assert(kcomp_filesystem_bind(1, KCOMP_FILESYSTEM_CONTRACT, KCOMP_FILESYSTEM_ABI, &binding) == 0);
        uint8_t data[1024]; size_t actual;
        memset(data, 0xa5, sizeof(data));
        struct kcomp_call_result result = kcomp_filesystem_read(&binding, 7, data, 1, &actual);
        assert(result.transport == 0 && result.method == 0 && actual == 1);
        assert(data[0] == 'A' && data[1] == 0xa5);
        result = kcomp_filesystem_read(&binding, 7, data, sizeof(data), &actual);
        assert(result.transport == 0 && result.method == 0 && actual == 3);
        assert(memcmp(data, "ABC", 3) == 0);
        assert(seen == (mode == 0 ? 1024 : 520));
        result = kcomp_filesystem_read(&binding, 7, data, 0, &actual);
        assert(result.transport == 0 && result.method == 0 && actual == 0);
    }
    invalid = 1;
    uint8_t unchanged = 0xa5; size_t actual = 42;
    struct kcomp_call_result result = kcomp_filesystem_read(&binding, 7, &unchanged, 1, &actual);
    assert(result.transport == -EPROTO && unchanged == 0xa5 && actual == 0);
    return 0;
}
