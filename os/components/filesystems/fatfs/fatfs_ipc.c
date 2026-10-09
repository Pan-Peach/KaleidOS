/* One owned Server Task. Generated business dispatch; owner/rollback stay here. */
#include "fatfs_internal.h"
#include "kcomp_ipc.h"
#include <errno.h>
#include <string.h>

static struct fatfs_file_slot *find_file(struct fatfs_state *state, uint64_t handle) {
    for (size_t i = 0; handle && i < FATFS_MAX_OPEN_FILES; ++i)
        if (state->files[i].handle == handle) return &state->files[i];
    return NULL;
}
static int consumer_live(uint32_t consumer, uint32_t task) {
    int32_t status = kcore_task_state(task);
    if (status < 0 || status == 4) return 0;
    uint8_t name[256]; struct kcore_component_info row;
    for (uint32_t i = 0; ; ++i) {
        int32_t rc = kcore_component_nth(i, &row, name, sizeof(name));
        if (rc) return 0;
        if (row.id == consumer) return row.state == 2 || row.state == 3;
    }
}
static void reap(struct fatfs_state *state) {
    for (size_t i = 0; i < FATFS_MAX_OPEN_FILES; ++i) {
        struct fatfs_file_slot *file = &state->files[i];
        if (file->handle && file->consumer && !consumer_live(file->consumer, file->consumer_task))
            fatfs_close(state, file->handle);
    }
}
void fatfs_server(void *arg) {
    struct fatfs_state *state = arg;
    uint32_t owner = 0; uint64_t endpoint = 0;
    if (kcore_component_current(&owner) || kcore_endpoint_lookup(owner,
        (const uint8_t *)KCOMP_FILESYSTEM_NAME, sizeof(KCOMP_FILESYSTEM_NAME) - 1,
        KCOMP_FILESYSTEM_CONTRACT, &endpoint) || kcore_ipc_listen(endpoint)) goto done;
    if (state->control && kcore_ipc_grant(endpoint, state->control)) goto done;
    uint8_t request[KCORE_IPC_MESSAGE_MAX], reply[KCORE_IPC_MESSAGE_MAX];
    for (;;) {
        uint64_t receipt = 0; uint32_t consumer = 0, task = 0; size_t len = 0;
        int32_t rc = kcore_ipc_receive(endpoint, request, sizeof(request), &receipt, &consumer, &task, &len);
        if (rc == -EAGAIN) {
            if (kcore_ipc_wait(endpoint, 0)) break;
            continue;
        }
        if (rc) break;
        reap(state);
        struct kcomp_ipc_request message;
        memset(reply, 0, sizeof(reply));
        rc = kcomp_ipc_decode(request, len, &message);
        size_t output_len = rc ? 0 : message.output_len;
        uint64_t created = 0;
        int shutdown = 0;
        if (!rc) {
            rc = kcomp_filesystem_wire_validate(&message);
            if (!rc && message.method == KCOMP_FILESYSTEM_METHOD_SHUTDOWN) {
                if (consumer != state->control) rc = -EACCES;
                else {
                    for (size_t i = 0; i < FATFS_MAX_OPEN_FILES; ++i)
                        if (state->files[i].handle) fatfs_close(state, state->files[i].handle);
                    rc = fatfs_unmount(state);
                    shutdown = !rc;
                }
            }
            /* The proxy opens Nodes; IPC never accepts a VFS absolute path. */
            else if (!rc && message.method == KCOMP_FILESYSTEM_METHOD_OPEN) rc = -ENOTSUP;
            else if (!rc && (message.method == KCOMP_FILESYSTEM_METHOD_CLOSE ||
                     message.method == KCOMP_FILESYSTEM_METHOD_READ ||
                     message.method == KCOMP_FILESYSTEM_METHOD_READ_AT)) {
                struct fatfs_file_slot *file = find_file(state, kcomp_ipc_u64(message.args));
                if (!file) rc = -EBADF;
                else if (file->consumer != consumer || file->consumer_task != task) rc = -EACCES;
            }
            if (!rc && !shutdown) {
                rc = kcomp_filesystem_wire_dispatch(state, &message, reply + KCOMP_REPLY_HEADER_LEN, output_len);
                if (!rc && message.method == KCOMP_FILESYSTEM_METHOD_OPEN_NODE) {
                    created = kcomp_ipc_u64(reply + KCOMP_REPLY_HEADER_LEN);
                    struct fatfs_file_slot *file = find_file(state, created);
                    if (!file) rc = -EIO;
                    else { file->consumer = consumer; file->consumer_task = task; }
                }
            }
        }
        kcomp_ipc_put32(reply, (uint32_t)rc);
        int32_t committed = kcore_ipc_reply(receipt, reply, KCOMP_REPLY_HEADER_LEN + output_len);
        /* Cancel/exit before reply retires the receipt and rolls back creation. */
        if (committed && created) fatfs_close(state, created);
        if (shutdown) { state->alive = 0; kcore_ipc_close(endpoint); break; }
    }
 done:
    kcore_task_exit();
}
