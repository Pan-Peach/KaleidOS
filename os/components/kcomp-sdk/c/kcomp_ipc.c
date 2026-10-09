#include "kcomp.h"
#include "kcomp_ipc.h"
#include <errno.h>
#include <string.h>
uint32_t kcomp_ipc_u32(const uint8_t *p) {
    uint32_t n = 0;
    for (size_t i = 0; i < 4; ++i) n |= (uint32_t)p[i] << (8 * i);
    return n;
}
uint64_t kcomp_ipc_u64(const uint8_t *p) {
    uint64_t n = 0;
    for (size_t i = 0; i < 8; ++i) n |= (uint64_t)p[i] << (8 * i);
    return n;
}
void kcomp_ipc_put32(uint8_t *p, uint32_t n) {
    for (size_t i = 0; i < 4; ++i) p[i] = (uint8_t)(n >> (8 * i));
}
void kcomp_ipc_put64(uint8_t *p, uint64_t n) {
    for (size_t i = 0; i < 8; ++i) p[i] = (uint8_t)(n >> (8 * i));
}
int32_t kcomp_ipc_decode(const uint8_t *p, size_t n, struct kcomp_ipc_request *out) {
    if (!p || !out || n < KCOMP_REQUEST_HEADER_LEN || n > KCORE_IPC_MESSAGE_MAX) return -EINVAL;
    size_t a = kcomp_ipc_u32(p + 8), b = kcomp_ipc_u32(p + 12);
    size_t capacity = kcomp_ipc_u32(p + 4);
    if (a > n - KCOMP_REQUEST_HEADER_LEN || b != n - KCOMP_REQUEST_HEADER_LEN - a ||
        capacity > KCORE_IPC_MESSAGE_MAX - KCOMP_REPLY_HEADER_LEN) return -EINVAL;
    *out = (struct kcomp_ipc_request){kcomp_ipc_u32(p), p + KCOMP_REQUEST_HEADER_LEN,
        a, p + KCOMP_REQUEST_HEADER_LEN + a, b, capacity};
    return 0;
}
int32_t kcomp_ipc_invoke(uint64_t endpoint, uint32_t method,
    const void *args, size_t a, const void *input, size_t b,
    void *output, size_t capacity, int32_t *status) {
    if (a > KCORE_IPC_MESSAGE_MAX - KCOMP_REQUEST_HEADER_LEN ||
        b > KCORE_IPC_MESSAGE_MAX - KCOMP_REQUEST_HEADER_LEN - a ||
        capacity > KCORE_IPC_MESSAGE_MAX - KCOMP_REPLY_HEADER_LEN) return -EMSGSIZE;
    if ((!args && a) || (!input && b) || (!output && capacity) || !status) return -EFAULT;
    uint8_t request[KCORE_IPC_MESSAGE_MAX], reply[KCORE_IPC_MESSAGE_MAX];
    kcomp_ipc_put32(request, method); kcomp_ipc_put32(request + 4, (uint32_t)capacity);
    kcomp_ipc_put32(request + 8, (uint32_t)a); kcomp_ipc_put32(request + 12, (uint32_t)b);
    if (a) memcpy(request + KCOMP_REQUEST_HEADER_LEN, args, a);
    if (b) memcpy(request + KCOMP_REQUEST_HEADER_LEN + a, input, b);
    uint64_t id; int32_t rc = kcore_ipc_submit(endpoint, request, KCOMP_REQUEST_HEADER_LEN + a + b, &id);
    if (rc) return rc;
    size_t len = 0; int32_t completion = 0;
    for (;;) {
        rc = kcore_ipc_collect(id, reply, sizeof(reply), &len, &completion);
        if (rc != -EAGAIN) break;
        rc = kcore_ipc_wait(0, id);
        if (rc) {
            kcore_ipc_cancel(id);
            kcore_ipc_collect(id, reply, sizeof(reply), &len, &completion);
            return rc;
        }
    }
    if (rc) return rc;
    if (completion) return completion;
    if (len != capacity + KCOMP_REPLY_HEADER_LEN) return -EPROTO;
    *status = (int32_t)kcomp_ipc_u32(reply);
    if (*status > 0) return -EPROTO;
    if (capacity) memcpy(output, reply + KCOMP_REPLY_HEADER_LEN, capacity);
    return 0;
}
