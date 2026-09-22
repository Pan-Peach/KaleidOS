#ifndef KALEIDOS_DISKIO_H
#define KALEIDOS_DISKIO_H

#include "kcomp.h"

int32_t fatfs_disk_attach(
    const struct kcomp_block_device_api *block,
    void *block_ctx);

void fatfs_disk_detach(void);

#endif
