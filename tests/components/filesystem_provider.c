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
static int pause_read, reading, resume;
static struct state_type state;
static uint64_t current;

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
    pthread_mutex_lock(&barrier);
    resume = 1; pthread_cond_broadcast(&changed);
    pthread_mutex_unlock(&barrier);
    assert(pthread_join(thread, NULL) == 0);
    assert(fs_close(&state, current) == 0);
    assert(fs_unmount(&state) == 0);
    puts("provider stale/empty/overflow/interleaving PASS");
    return 0;
}
