#ifndef KALEIDOS_DISKIO_H
#define KALEIDOS_DISKIO_H

#include "kcomp.h"

/* 挂上/摘下活跃块绑定。绑定本身由 create config 里的 EndpointId 经
 * `kcomp_block_bind` 构造（见 fatfs.c）；disk glue 只持指针、经统一包装调用。 */
int32_t fatfs_disk_attach(const struct kcomp_block_binding *block);

void fatfs_disk_detach(void);

#endif
