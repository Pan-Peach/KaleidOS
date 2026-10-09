#ifndef KCOMP_IPC_H
#define KCOMP_IPC_H
#include <stddef.h>
#include <stdint.h>
/* SDK envelope only; Core transports opaque bytes. */
struct kcomp_ipc_request {
    uint32_t method;
    const uint8_t *args;
    size_t args_len;
    const uint8_t *input;
    size_t input_len;
    size_t output_len;
};
int32_t kcomp_ipc_decode(const uint8_t *bytes, size_t len, struct kcomp_ipc_request *out);
int32_t kcomp_ipc_invoke(uint64_t endpoint, uint32_t method,
    const void *args, size_t args_len, const void *input, size_t input_len,
    void *output, size_t output_len, int32_t *method_status);
uint32_t kcomp_ipc_u32(const uint8_t *bytes);
uint64_t kcomp_ipc_u64(const uint8_t *bytes);
void kcomp_ipc_put32(uint8_t *bytes, uint32_t value);
void kcomp_ipc_put64(uint8_t *bytes, uint64_t value);
#endif
