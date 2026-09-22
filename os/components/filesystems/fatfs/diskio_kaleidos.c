
#include "kcomp.h"
#include "ff.h"
#include "diskio.h"
#include <errno.h>

#include "diskio_kaleidos.h"

static const struct kcomp_block_device_api *active_block;
static void *active_block_ctx;

int32_t fatfs_disk_attach(
    const struct kcomp_block_device_api *block,
    void *block_ctx)
{
    if (block == NULL ||
        block->capacity_sectors == NULL || block->read == NULL)
    {
        return -EINVAL;
    }

    if (active_block != NULL)
    {
        return -EBUSY;
    }

    active_block = block;
    active_block_ctx = block_ctx;
    return 0;
}

void fatfs_disk_detach(void)
{
    active_block = NULL;
    active_block_ctx = NULL;
}

static int valid_drive(BYTE pdrv)
{
    return (pdrv == 0 && active_block != NULL);
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
    uint64_t capacity = active_block->capacity_sectors(active_block_ctx);

    if (start >= capacity || sectors > capacity - start)
    {
        return RES_PARERR;
    }

    int32_t result = active_block->read(active_block_ctx, start, buf, length);

    if (result < 0)
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
            uint64_t capacity = active_block->capacity_sectors(active_block_ctx);
            if (buff == NULL || capacity > UINT32_MAX)
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
