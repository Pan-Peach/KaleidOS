#ifndef KCOMP_VFS_H
#define KCOMP_VFS_H

/* VFS 契约声明；Direct/Gate 适配器尚未实现。
 * 语义 / flat wire 见 docs/interfaces/vfs.md，布局来源 abi/vfs.toml。
 * 输入指针仅本次调用借用；用户态 VA 不可直接传入 table。
 */
#include "generated/kcomp_abi.h"

#endif
