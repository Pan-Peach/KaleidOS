/* lfs_adapter.c —— littlefs 的宿主回调 → block.device（组件内 adapter，§3）。
 *
 * 上游 lfs.c 对 KaleidOS 一无所知：它只认 `struct lfs_config` 的原生回调；
 * 回调在本文件里翻译成 `kcomp_block_read` / `kcomp_block_write` 的统一调用。
 * 本文件是**唯一**把 littlefs 接到 block.device 的地方（thin adapter 规则）。
 *
 * 几何：block_size = 512 = block.device 的 sector，因此 1 个 littlefs block 恰好
 * 1 个 sector。littlefs 以 read_size/prog_size（16 字节）粒度访问，而 block.device
 * 只接受 512 字节整数倍——非整 sector 的访问经 state->sector 做 bounce；prog 额外
 * 做 read-modify-write（littlefs 只向已擦除区域 prog，保留的其余字节因此仍是
 * 0xFF）。erase 没有对应 op：整块写 0xFF（flash 擦除态）。
 */
#include "kcomp.h"
#include "lfs_adapter.h"
#include <errno.h>
#include <string.h>

/* 块调用的公共收口：传输状态优先；provider 的方法状态必须是 0 / -errno
 * （正数 = 契约违约，不猜成成功）。 */
static int littlefs_block_result(struct kcomp_call_result result)
{
    if (result.transport < 0)
    {
        return (int)result.transport;
    }

    if (result.method < 0)
    {
        return (int)result.method;
    }

    if (result.method != 0)
    {
        return -EIO;
    }

    return 0;
}

/* 读一个整 sector（512 字节）到 out。 */
static int littlefs_sector_read(struct littlefs_state *state, uint64_t sector, uint8_t *out)
{
    return littlefs_block_result(
        kcomp_block_read(&state->block_binding, sector, out, KCOMP_BLOCK_DEVICE_SECTOR));
}

/* 写一个整 sector（512 字节）。 */
static int littlefs_sector_write(struct littlefs_state *state, uint64_t sector, const uint8_t *in)
{
    return littlefs_block_result(
        kcomp_block_write(&state->block_binding, sector, in, KCOMP_BLOCK_DEVICE_SECTOR));
}

/* 读 [block, off, size)：按 512 字节 sector 切片；整 sector 直读，否则经 bounce。 */
static int littlefs_adapter_read(
    const struct lfs_config *cfg,
    lfs_block_t block,
    lfs_off_t off,
    void *buffer,
    lfs_size_t size)
{
    struct littlefs_state *state = cfg->context;
    uint64_t base = (uint64_t)block * cfg->block_size + off;
    uint8_t *out = buffer;

    while (size > 0)
    {
        uint64_t sector = base / KCOMP_BLOCK_DEVICE_SECTOR;
        size_t sector_off = (size_t)(base % KCOMP_BLOCK_DEVICE_SECTOR);
        size_t chunk = KCOMP_BLOCK_DEVICE_SECTOR - sector_off;

        if (chunk > size)
        {
            chunk = size;
        }

        if (sector_off == 0 && chunk == KCOMP_BLOCK_DEVICE_SECTOR)
        {
            int result = littlefs_sector_read(state, sector, out);
            if (result < 0)
            {
                return result;
            }
        }
        else
        {
            int result = littlefs_sector_read(state, sector, state->sector);
            if (result < 0)
            {
                return result;
            }
            memcpy(out, state->sector + sector_off, chunk);
        }

        out += chunk;
        base += chunk;
        size -= chunk;
    }

    return 0;
}

/* 写 [block, off, size)：整 sector 直写；否则 read-modify-write。 */
static int littlefs_adapter_prog(
    const struct lfs_config *cfg,
    lfs_block_t block,
    lfs_off_t off,
    const void *buffer,
    lfs_size_t size)
{
    struct littlefs_state *state = cfg->context;
    uint64_t base = (uint64_t)block * cfg->block_size + off;
    const uint8_t *in = buffer;

    while (size > 0)
    {
        uint64_t sector = base / KCOMP_BLOCK_DEVICE_SECTOR;
        size_t sector_off = (size_t)(base % KCOMP_BLOCK_DEVICE_SECTOR);
        size_t chunk = KCOMP_BLOCK_DEVICE_SECTOR - sector_off;
        int result;

        if (chunk > size)
        {
            chunk = size;
        }

        if (sector_off == 0 && chunk == KCOMP_BLOCK_DEVICE_SECTOR)
        {
            result = littlefs_sector_write(state, sector, in);
        }
        else
        {
            /* block.device 只接受整 sector 写：read-modify-write。littlefs 只向
             * 已擦除区域 prog，RMW 保留的其余字节因此仍是 0xFF。 */
            result = littlefs_sector_read(state, sector, state->sector);
            if (result < 0)
            {
                return result;
            }
            memcpy(state->sector + sector_off, in, chunk);
            result = littlefs_sector_write(state, sector, state->sector);
        }

        if (result < 0)
        {
            return result;
        }

        in += chunk;
        base += chunk;
        size -= chunk;
    }

    return 0;
}

/* 擦除一个 block：block.device 没有 erase op，整块写 0xFF。 */
static int littlefs_adapter_erase(const struct lfs_config *cfg, lfs_block_t block)
{
    struct littlefs_state *state = cfg->context;
    uint64_t base = (uint64_t)block * cfg->block_size;
    lfs_size_t remaining = cfg->block_size;

    while (remaining > 0)
    {
        uint64_t sector = base / KCOMP_BLOCK_DEVICE_SECTOR;
        size_t sector_off = (size_t)(base % KCOMP_BLOCK_DEVICE_SECTOR);
        size_t chunk = KCOMP_BLOCK_DEVICE_SECTOR - sector_off;
        int result;

        if (chunk > remaining)
        {
            chunk = remaining;
        }

        if (sector_off == 0 && chunk == KCOMP_BLOCK_DEVICE_SECTOR)
        {
            memset(state->sector, 0xFF, sizeof(state->sector));
            result = littlefs_sector_write(state, sector, state->sector);
        }
        else
        {
            result = littlefs_sector_read(state, sector, state->sector);
            if (result < 0)
            {
                return result;
            }
            memset(state->sector + sector_off, 0xFF, chunk);
            result = littlefs_sector_write(state, sector, state->sector);
        }

        if (result < 0)
        {
            return result;
        }

        base += chunk;
        remaining -= (lfs_size_t)chunk;
    }

    return 0;
}

static int littlefs_adapter_sync(const struct lfs_config *cfg)
{
    (void)cfg;

    /* block.device 的 read / write 阻塞到传输完成（无设备侧写缓存），没有需要
     * flush 的状态。 */
    return 0;
}

void littlefs_adapter_init(struct littlefs_state *state)
{
    struct lfs_config *config = &state->config;

    memset(config, 0, sizeof(*config));
    config->context = state;
    config->read = littlefs_adapter_read;
    config->prog = littlefs_adapter_prog;
    config->erase = littlefs_adapter_erase;
    config->sync = littlefs_adapter_sync;
    config->read_size = LITTLEFS_READ_SIZE;
    config->prog_size = LITTLEFS_PROG_SIZE;
    config->block_size = LITTLEFS_BLOCK_SIZE;
    config->block_count = 0; /* mount 前按设备容量填（littlefs_adapter_capacity） */
    config->block_cycles = LITTLEFS_BLOCK_CYCLES;
    config->cache_size = LITTLEFS_CACHE_SIZE;
    config->lookahead_size = LITTLEFS_LOOKAHEAD_SIZE;
    config->read_buffer = state->read_buffer;
    config->prog_buffer = state->prog_buffer;
    config->lookahead_buffer = state->lookahead_buffer;
}

int32_t littlefs_adapter_capacity(struct littlefs_state *state, uint64_t *out_sectors)
{
    if (state == NULL || out_sectors == NULL)
    {
        return -EINVAL;
    }

    struct kcomp_call_result result = kcomp_block_capacity(&state->block_binding, out_sectors);
    if (result.transport < 0)
    {
        return result.transport;
    }

    if (result.method != 0)
    {
        return -EIO;
    }

    return 0;
}
