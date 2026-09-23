#include "kcomp.h"
#include "ff.h"
#include "diskio.h"
#include <errno.h>

#include "diskio_kaleidos.h"

/* 活跃块绑定：组合策略经 create config 交付 opaque EndpointId，create 里
 * `kcomp_block_bind` 把它变成绑定（机制由 Core 选定，藏在绑定内部）。
 * disk glue 只经统一包装 `kcomp_block_capacity` / `kcomp_block_read` 调用——
 * 不持有裸 function table，也不按机制分支。 */
static const struct kcomp_block_binding *active_block;

int32_t fatfs_disk_attach(const struct kcomp_block_binding *block)
{
    if (block == NULL)
    {
        return -EINVAL;
    }

    if (active_block != NULL)
    {
        return -EBUSY;
    }

    active_block = block;
    return 0;
}

void fatfs_disk_detach(void)
{
    active_block = NULL;
}

static int valid_drive(BYTE pdrv)
{
    return (pdrv == 0 && active_block != NULL);
}

/* 容量查询的公共收口：传输状态 / 方法状态都必须干净。 */
static int32_t block_capacity(uint64_t *out_sectors)
{
    struct kcomp_call_result result = kcomp_block_capacity(active_block, out_sectors);

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

DSTATUS disk_status(BYTE pdrv)
{
    if (!valid_drive(pdrv))
    {
        return STA_NOINIT;
    }

    return 0;
}

DSTATUS disk_initialize(BYTE pdrv)
{
    return disk_status(pdrv);
}

DRESULT disk_read(
    BYTE pdrv,
    BYTE *buf,
    LBA_t sector,
    UINT count)
{
    if (!valid_drive(pdrv))
    {
        return RES_NOTRDY;
    }

    if (buf == 0 || count == 0)
    {
        return RES_PARERR;
    }

    if ((size_t)count > ((size_t)-1) / KCOMP_BLOCK_DEVICE_SECTOR)
    {
        return RES_PARERR;
    }

    size_t length = (size_t)count * KCOMP_BLOCK_DEVICE_SECTOR;

    uint64_t start = (uint64_t)sector;
    uint64_t sectors = (uint64_t)count;
    uint64_t capacity = 0;

    if (block_capacity(&capacity) < 0)
    {
        return RES_ERROR;
    }

    if (start >= capacity || sectors > capacity - start)
    {
        return RES_PARERR;
    }

    struct kcomp_call_result result = kcomp_block_read(active_block, start, buf, length);

    if (result.transport < 0 || result.method != 0)
    {
        return RES_ERROR;
    }

    return RES_OK;
}

DRESULT disk_write(
    BYTE pdrv,
    const BYTE *buf,
    LBA_t sector,
    UINT count)
{
    (void)buf;
    (void)sector;
    (void)count;

    if (!valid_drive(pdrv))
    {
        return RES_NOTRDY;
    }

    return RES_WRPRT;
}

DRESULT disk_ioctl(
    BYTE pdrv,
    BYTE cmd,
    void *buff)
{
    if (!valid_drive(pdrv))
    {
        return RES_NOTRDY;
    }

    switch (cmd)
    {
        case CTRL_SYNC:
            return RES_OK;

        case GET_SECTOR_COUNT:
        {
            uint64_t capacity = 0;

            if (buff == NULL)
            {
                return RES_PARERR;
            }

            if (block_capacity(&capacity) < 0)
            {
                return RES_ERROR;
            }

            if (capacity > UINT32_MAX)
            {
                return RES_PARERR;
            }

            *(DWORD *)buff = (DWORD)capacity;
            return RES_OK;
        }

        case GET_SECTOR_SIZE:
            if (buff == NULL)
            {
                return RES_PARERR;
            }

            *(WORD *)buff = KCOMP_BLOCK_DEVICE_SECTOR;
            return RES_OK;

        default:
            return RES_PARERR;
    }
}
