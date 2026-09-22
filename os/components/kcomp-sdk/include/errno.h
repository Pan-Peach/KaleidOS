/* errno.h —— freestanding C 组件的错误码（POSIX/Linux 数值；**不是 libc**）。
 *
 * clang `-ffreestanding` 不提供 <errno.h>。本 shim 是**手写 umbrella**：数值本体在
 * generated/errno.h（由 tools/kabi/kabi_gen.py 从 abi/errno.toml 生成，单一来源；
 * 与 Core `os/core/src/generated/errno.rs`、SDK `src/generated/errno.rs` 同源）。
 * 之所以保留本文件，是因为 tools/build-kcomp-c.sh 只把 include/ 加进 -I，
 * `#include <errno.h>` 必须在这里可解析。
 *
 * ABI 约定：函数返回 `0` 或 **`-errno`**，所以组件写法是 `return -ENODEV;`。
 * （95 的别名 EOPNOTSUPP 与 ENOTSUP 同码，这里只给 ENOTSUP。）
 */
#ifndef KCOMP_ERRNO_H
#define KCOMP_ERRNO_H

#include "generated/errno.h"

#endif /* KCOMP_ERRNO_H */
