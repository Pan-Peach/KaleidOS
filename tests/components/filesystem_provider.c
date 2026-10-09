/* Host test of production provider/library code; the block medium is a fake. */
#include <assert.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#ifdef TEST_FATFS
#include "fatfs_internal.h"
#include "diskio_kaleidos.h"
#define state_type fatfs_state
#define fs_mount fatfs_mount
#define fs_unmount fatfs_unmount
#define fs_open fatfs_open
#define fs_read fatfs_read
#define fs_close fatfs_close
#define path "0:/HELLO.TXT"
#else
#include "littlefs_internal.h"
#include "lfs_adapter.h"
#define state_type littlefs_state
#define fs_mount littlefs_mount
#define fs_unmount littlefs_unmount
#define fs_open littlefs_open
#define fs_read littlefs_read
#define fs_close littlefs_close
#define path "selftest.txt"
#endif
#include <errno.h>

static uint8_t disk[1024 * 1024];
static pthread_mutex_t barrier = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t changed = PTHREAD_COND_INITIALIZER;
static int pause_read, reading, resume, fail_read;
static struct state_type state;
static uint64_t current;

#ifdef TEST_FATFS
int32_t kcomp_service_dispatch(void *, uint32_t, uint32_t, const struct kcomp_call_frame *);
static uint32_t mechanism;
static const struct kcomp_filesystem_api api = {
    .mount = fatfs_mount, .unmount = fatfs_unmount, .open = fatfs_open,
    .read = fatfs_read, .close = fatfs_close,
    .root = fatfs_root, .lookup = fatfs_lookup, .node_info = fatfs_node_info,
};

int32_t kcore_endpoint_bind(uint64_t endpoint, uint64_t contract, uint64_t abi,
                             uint32_t *out_mechanism, size_t *out_api, size_t *out_ctx)
{
    assert(endpoint == 1 && contract == KCOMP_FILESYSTEM_CONTRACT && abi == KCOMP_FILESYSTEM_ABI);
    *out_mechanism = mechanism;
    *out_api = (size_t)&api;
    *out_ctx = (size_t)&state;
    return 0;
}

int32_t kcore_endpoint_call(uint64_t endpoint, uint32_t method, const uint8_t *args,
                            size_t args_len, const uint8_t *input, size_t input_len,
                            uint8_t *output, size_t output_len, int32_t *out_status)
{
    assert(endpoint == 1 && mechanism == KCORE_ENDPOINT_MECHANISM_GATE);
    struct kcomp_call_frame frame = {args, args_len, input, input_len, output, output_len};
    *out_status = kcomp_service_dispatch(&state, FATFS_PORT, method, &frame);
    return 0;
}

static void expect_method(struct kcomp_call_result result, int32_t status)
{
    assert(result.transport == 0 && result.method == status);
}

static struct kcomp_call_result lookup(const struct kcomp_filesystem_binding *binding,
                                       uint64_t parent, const char *name, uint64_t *out)
{
    return kcomp_filesystem_lookup(binding, parent, (const uint8_t *)name, strlen(name),
                                   KCOMP_FILESYSTEM_ENCODING_BYTES, out);
}

static void test_lookup(void)
{
    /* Both SDK transports reach the production backend and the real FatFs library. */
    for (mechanism = KCORE_ENDPOINT_MECHANISM_DIRECT;
         mechanism <= KCORE_ENDPOINT_MECHANISM_GATE; ++mechanism) {
        struct kcomp_filesystem_binding binding;
        assert(kcomp_filesystem_bind(1, KCOMP_FILESYSTEM_CONTRACT, KCOMP_FILESYSTEM_ABI, &binding) == 0);
        uint64_t root = 0, file = 0, alias = 0, dir1 = 0, dir2 = 0, child1 = 0, child2 = 0;
        uint32_t kind = 0;
        expect_method(kcomp_filesystem_root(&binding, &root), 0);
        assert(root != 0);
        expect_method(kcomp_filesystem_root(&binding, &alias), 0);
        assert(alias == root);
        expect_method(kcomp_filesystem_node_info(&binding, root, &kind), 0);
        assert(kind == KCOMP_FILESYSTEM_NODE_DIRECTORY);
        expect_method(lookup(&binding, root, "MISSING", &file), -ENOENT);
        assert(file == 0 && state.last_node == root);
        expect_method(lookup(&binding, root, "hello.txt", &file), 0);
        expect_method(lookup(&binding, root, "HELLO.TXT", &alias), 0);
        assert(file != root && file == alias);
        expect_method(kcomp_filesystem_node_info(&binding, file, &kind), 0);
        assert(kind == KCOMP_FILESYSTEM_NODE_FILE);
        uint8_t details[28];
        assert(fatfs_node_details(&state, file, details) == 0);
        assert(details[0] == KCOMP_FILESYSTEM_NODE_FILE && details[4] == 9);
        assert(memcmp(details + 16, "HELLO.TXT", 9) == 0);
        uint64_t opened = 0;
        assert(fatfs_open_node(&state, root, &opened) == -EISDIR && opened == 0);
        assert(fatfs_open_node(&state, file, &opened) == 0 && opened != 0);
        uint8_t bytes[5]; size_t count = 999;
        assert(fatfs_read_at(&state, opened, 6, bytes, sizeof(bytes), &count) == 0);
        assert(count == 5 && memcmp(bytes, "FROM ", 5) == 0);
        assert(fatfs_read(&state, opened, bytes, sizeof(bytes), &count) == 0);
        assert(count == 5 && memcmp(bytes, "HELLO", 5) == 0);
        assert(fatfs_read_at(&state, opened, UINT64_MAX, bytes, sizeof(bytes), &count) == -EOVERFLOW);
        assert(count == 0);
        assert(fatfs_close(&state, opened) == 0);
        assert(fatfs_read_at(&state, opened, 0, bytes, sizeof(bytes), &count) == -EBADF);
        expect_method(lookup(&binding, file, "HELLO.TXT", &alias), -ENOTDIR);
        expect_method(lookup(&binding, 0, "HELLO.TXT", &alias), -EBADF);
        expect_method(lookup(&binding, UINT64_MAX, "HELLO.TXT", &alias), -EBADF);
        expect_method(kcomp_filesystem_node_info(&binding, UINT64_MAX, &kind), -EBADF);

        const char *invalid[] = {".", "..", "DIR1/HELLO.TXT", "DIR1\\HELLO.TXT",
                                 "HELLO.TXT.", " HELLO.TXT", "HELLO.TXT ", "0:HELLO.TXT",
                                 "HELLO.TXTT", "123456789", "A..B", "A?B", "A*B", ".TXT"};
        for (size_t i = 0; i < sizeof(invalid) / sizeof(invalid[0]); ++i)
            expect_method(lookup(&binding, root, invalid[i], &alias), -EINVAL);
        const uint8_t nul[] = {'A', 0, 'B'};
        expect_method(kcomp_filesystem_lookup(&binding, root, nul, sizeof(nul), 1, &alias), -EINVAL);
        const uint8_t high[] = {0x81};
        expect_method(kcomp_filesystem_lookup(&binding, root, high, 1, 1, &alias), -ENOTSUP);
        expect_method(kcomp_filesystem_lookup(&binding, root, (const uint8_t *)"A", 1, 2, &alias), -ENOTSUP);
        assert(kcomp_filesystem_lookup(&binding, root, nul, 0, 1, &alias).transport == -EINVAL);
        assert(fatfs_lookup(&state, root, NULL, 1, 1, &alias) == -EINVAL);
        assert(fatfs_lookup(&state, root, nul, sizeof(nul), 1, NULL) == -EINVAL);

        expect_method(lookup(&binding, root, "dir1", &dir1), 0);
        expect_method(lookup(&binding, root, "DIR2", &dir2), 0);
        expect_method(lookup(&binding, dir1, "hello.txt", &child1), 0);
        expect_method(lookup(&binding, dir2, "HELLO.TXT", &child2), 0);
        assert(child1 != child2 && child1 != file && child2 != file);
        expect_method(lookup(&binding, root, "DIR1", &alias), 0);
        assert(alias == dir1);

        uint64_t saved = state.last_node;
        state.last_node = UINT64_MAX;
        expect_method(lookup(&binding, root, "N1", &alias), -EOVERFLOW);
        expect_method(lookup(&binding, root, "HELLO.TXT", &alias), 0);
        assert(alias == file);
        state.last_node = saved;
        /* Six Nodes above; exercise the production budget rather than a fixed 8. */
        char capacity_name[16];
        for (size_t i = 1; i <= FATFS_MAX_NODES - 6; ++i) {
            snprintf(capacity_name, sizeof(capacity_name), "N%zu", i);
            expect_method(lookup(&binding, root, capacity_name, &alias), 0);
        }
        snprintf(capacity_name, sizeof(capacity_name), "N%u", FATFS_MAX_NODES - 5);
        expect_method(lookup(&binding, root, capacity_name, &alias), -ENOSPC);
        expect_method(lookup(&binding, root, "MISSING", &alias), -ENOENT);
        expect_method(lookup(&binding, root, "HELLO.TXT", &alias), 0);
        assert(alias == file);

        expect_method(kcomp_filesystem_unmount(&binding), 0);
        expect_method(kcomp_filesystem_root(&binding, &alias), -ENODEV);
        expect_method(lookup(&binding, root, "HELLO.TXT", &alias), -ENODEV);
        expect_method(kcomp_filesystem_node_info(&binding, file, &kind), -ENODEV);
        expect_method(kcomp_filesystem_mount(&binding), 0);
        expect_method(kcomp_filesystem_root(&binding, &alias), 0);
        assert(alias != root);
        expect_method(lookup(&binding, root, "HELLO.TXT", &alias), -EBADF);
        expect_method(kcomp_filesystem_node_info(&binding, file, &kind), -EBADF);
    }

    /* Malformed Gate frames never mutate the node table or publish an output. */
    uint8_t args[12] = {0}, output[8];
    memset(output, 0xa5, sizeof(output));
    uint64_t saved = state.last_node;
    struct kcomp_call_frame frame = {args, 11, (const uint8_t *)"N1", 2, output, 8};
    assert(kcomp_service_dispatch(&state, FATFS_PORT, KCOMP_FILESYSTEM_METHOD_LOOKUP, &frame) == -EINVAL);
    frame.args_len = 12; frame.output_len = 7;
    assert(kcomp_service_dispatch(&state, FATFS_PORT, KCOMP_FILESYSTEM_METHOD_LOOKUP, &frame) == -EINVAL);
    frame.output_len = 8; frame.args = NULL;
    assert(kcomp_service_dispatch(&state, FATFS_PORT, KCOMP_FILESYSTEM_METHOD_LOOKUP, &frame) == -EINVAL);
    frame.args_len = 0; frame.input_len = 1;
    assert(kcomp_service_dispatch(&state, FATFS_PORT, KCOMP_FILESYSTEM_METHOD_ROOT, &frame) == -EINVAL);
    frame.args = args; frame.args_len = 8; frame.input_len = 0; frame.output_len = 3;
    assert(kcomp_service_dispatch(&state, FATFS_PORT, KCOMP_FILESYSTEM_METHOD_NODE_INFO, &frame) == -EINVAL);
    assert(state.last_node == saved);
    for (size_t i = 0; i < sizeof(output); ++i) assert(output[i] == 0xa5);
    uint64_t node = 0; uint32_t kind = 0;
    fail_read = 1;
    assert(fatfs_lookup(&state, state.nodes[0].id, (const uint8_t *)"N1", 2, 1, &node) == -EIO);
    assert(node == 0 && state.last_node == saved);
    fail_read = 0;
    assert(fatfs_lookup(&state, state.nodes[0].id, (const uint8_t *)"N1", 2, 1, &node) == 0);
    state.alive = 0;
    assert(fatfs_root(&state, &node) == -ENODEV);
    assert(fatfs_lookup(&state, 1, (const uint8_t *)"N1", 2, 1, &node) == -ENODEV);
    assert(fatfs_node_info(&state, 1, &kind) == -ENODEV);
    state.alive = 1;
    assert(fatfs_unmount(&state) == 0);
    saved = state.last_node;
    state.last_node = UINT64_MAX;
    assert(fatfs_mount(&state) == -EOVERFLOW && !state.mounted);
    state.last_node = saved;
    assert(fatfs_mount(&state) == 0);
    puts("provider lookup/direct/gate/stale/capacity PASS");
}
#endif

int32_t kcore_log_line(const uint8_t *ptr, size_t len) { (void)ptr; (void)len; return 0; }
struct kcomp_call_result kcomp_block_capacity(const struct kcomp_block_binding *binding, uint64_t *out)
{
    (void)binding;
    *out = sizeof(disk) / 512;
    return (struct kcomp_call_result){0, 0};
}
struct kcomp_call_result kcomp_block_read(const struct kcomp_block_binding *binding,
                                         uint64_t lba, void *buf, size_t len)
{
    (void)binding;
    if (fail_read)
        return (struct kcomp_call_result){0, -EIO};
    pthread_mutex_lock(&barrier);
    if (pause_read) {
        reading = 1;
        pthread_cond_broadcast(&changed);
        while (!resume) pthread_cond_wait(&changed, &barrier);
        pause_read = 0;
    }
    pthread_mutex_unlock(&barrier);
    assert(lba < sizeof(disk) / 512 && len <= sizeof(disk) - lba * 512);
    memcpy(buf, disk + lba * 512, len);
    return (struct kcomp_call_result){0, 0};
}
struct kcomp_call_result kcomp_block_write(const struct kcomp_block_binding *binding,
                                          uint64_t lba, const void *buf, size_t len)
{
    (void)binding;
    assert(lba < sizeof(disk) / 512 && len <= sizeof(disk) - lba * 512);
    memcpy(disk + lba * 512, buf, len);
    return (struct kcomp_call_result){0, 0};
}
static void *reader(void *arg)
{
    (void)arg;
    uint8_t buf[64]; size_t actual = 0;
    assert(fs_read(&state, current, buf, sizeof(buf), &actual) == 0);
    assert(actual > 0);
    return NULL;
}
int main(int argc, char **argv)
{
#ifdef TEST_FATFS
    assert(argc == 2);
    FILE *image = fopen(argv[1], "rb"); assert(image);
    assert(fread(disk, 1, sizeof(disk), image) == sizeof(disk)); fclose(image);
    assert(fatfs_disk_attach(&state.block_binding) == 0);
#else
    (void)argc; (void)argv;
    memset(disk, 0xff, sizeof(disk));
    littlefs_adapter_init(&state);
#endif
    state.alive = 1;
    assert(fs_mount(&state) == 0);
#ifdef TEST_FATFS
    test_lookup();
#endif
    uint64_t stale = 0;
    assert(fs_open(&state, path, KCOMP_FILESYSTEM_OPEN_READ, &stale) == 0);
    assert(fs_close(&state, stale) == 0);
    assert(fs_open(&state, path, KCOMP_FILESYSTEM_OPEN_READ, &current) == 0);
    assert(current != stale && current != 0);
    uint8_t buf[1]; size_t actual = 999;
    assert(fs_read(&state, stale, buf, 0, &actual) == -EBADF);
    assert(fs_close(&state, stale) == -EBADF);
    assert(fs_read(&state, current, buf, 0, &actual) == 0 && actual == 0);
    assert(fs_unmount(&state) == -EBUSY);
    state.last_handle = UINT64_MAX;
    uint64_t overflow = 0;
    assert(fs_open(&state, path, KCOMP_FILESYSTEM_OPEN_READ, &overflow) == -EOVERFLOW);

    /* Force reader to remain inside real library I/O. No probabilistic race. */
    pause_read = 1;
    pthread_t thread; assert(pthread_create(&thread, NULL, reader, NULL) == 0);
    pthread_mutex_lock(&barrier);
    while (!reading) pthread_cond_wait(&changed, &barrier);
    pthread_mutex_unlock(&barrier);
    assert(fs_close(&state, current) == -EBUSY);
    assert(fs_mount(&state) == -EBUSY);
#ifdef TEST_FATFS
    uint64_t node = 0; uint32_t kind = 0;
    assert(fatfs_root(&state, &node) == -EBUSY);
    assert(fatfs_lookup(&state, state.nodes[0].id, (const uint8_t *)"N1", 2, 1, &node) == -EBUSY);
    assert(fatfs_node_info(&state, state.nodes[0].id, &kind) == -EBUSY);
#endif
    pthread_mutex_lock(&barrier);
    resume = 1; pthread_cond_broadcast(&changed);
    pthread_mutex_unlock(&barrier);
    assert(pthread_join(thread, NULL) == 0);
    assert(fs_close(&state, current) == 0);
    assert(fs_unmount(&state) == 0);
    puts("provider stale/empty/overflow/interleaving PASS");
    return 0;
}
