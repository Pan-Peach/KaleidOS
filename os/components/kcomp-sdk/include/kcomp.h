/* kcomp.h —— 组件 ABI 的 C 侧作者面（umbrella；声明本体在 abi/ 的 schema）。
 *
 * 边界两半：
 *   - `kcore_*` 导出 / 组件生命周期入口 / 稳定结构 / 常量 / 接口分类枚举：
 *     由 tools/kabi/kabi_gen.py 从 `abi/component.toml` + `abi/core.toml` 生成到
 *     `generated/kcomp_abi.h`，本文件只 include 它（生成物带布局 `_Static_assert`）；
 *   - `kcomp_*` 组件间契约（block.device / filesystem function table）：仍手写
 *     在这里（Phase 3 迁移到 schema）。
 *
 * Rust 镜像：`os/components/kcomp-sdk/src/abi.rs`（手写 facade）+
 * `src/generated/abi.rs`（生成物）；Core 侧 `os/core/src/generated/abi.rs` +
 * `os/core/src/component/generated/exports.rs`（typed 导出注册表）。
 * **改 ABI = 改 abi/ 的 schema**，然后 `make abi-gen`。
 *
 * 约定（docs/architecture/component-lifecycle.md）：
 *   - 返回值统一 `0 / -errno`；旧的"非零 = 失败 bitmap"约定已废弃。
 *   - 宽度：Rust `usize` ↔ C `size_t`；counts/ids → `uint32_t`；不透明句柄 →
 *     `uint64_t`；布尔 / 编码 → `int32_t`；长度 / 指针宽的 out → `size_t`。
 *   - 标识符不带版本后缀；契约变了就原地替换，不保留旧名（无 legacy fallback）。
 *   - C 是根：Rust ABI 永不成为组件 ABI。本头文件同时给 RV64 / RV32 编译，
 *     因此指针宽度相关的 `_Static_assert` 按 `__SIZEOF_POINTER__` 分别钉住。
 */
#ifndef KCOMP_H
#define KCOMP_H

#include <stddef.h>
#include <stdint.h>

#include "generated/kcomp_abi.h"

#ifdef __cplusplus
extern "C" {
#endif

/* ===========================================================================
 * 组件间契约（C）—— Interface Registry 的 function table
 * ===========================================================================
 *
 * 这些**不是** Core 导出：Core 只把 publish 进来的 `api` / `ctx` 当不透明指针
 * 存着，不认识契约语义。所以名字是 `kcomp_*`（组件面），不是 `kcore_*`。
 * 机制本身用已有导出即可：consumer 调 `kcore_interface_bind` 拿 api/ctx，
 * provider 调 `kcore_interface_publish` 交付 function table。
 *
 * provider 与 consumer 编译**同一份**契约；Core 在 bind 时 exact-compare ABI
 * 指纹。布局必须与 Rust 侧（`src/block.rs` 的 `#[repr(C)]` struct）逐字段一致
 * ——不一致 = 跨组件 UB，由 os/core/tests/kcomp_abi_drift.rs 钉住。
 */

/* -- block.device（provider：驱动如 virtio_blk；consumer：FS 如 FatFs）-- */

/* 接口的稳定名字（publish / bind 必须逐字节一致）。 */
#define KCOMP_BLOCK_DEVICE_NAME "block.device"
/* exact ABI fingerprint：8 字节 ASCII "BLOCKDEV" 的大端读数。 */
#define KCOMP_BLOCK_DEVICE_ABI UINT64_C(0x424C4F434B444556)
/* 契约单位：1 sector = 512 字节（`read`/`write` 的 len 必须是它的整数倍）。 */
#define KCOMP_BLOCK_DEVICE_SECTOR 512

/* BlockDevice 的 function table（provider/consumer 共享布局）。
 *
 * 契约（两个实现能否互通全看这几条）：
 *   - 单位：`lba` 以 512 字节 sector 计；`len` 是字节数，必须是 512 的整数倍。
 *   - 同步：`read`/`write` 阻塞到本次传输完成（当前实现轮询设备），调用方不得
 *     处于不能阻塞的上下文。
 *   - 上下文：只在 task 上下文调用；禁止 trap / 中断上下文。
 *   - 返回：`0` = 成功，`-errno` = 失败；非法参数（buf 为 null / len 为 0 或非
 *     512 整数倍）→ `-EINVAL`。
 *   - buffer：`buf` 指向 Core 可见 RAM（v1 无 IOMMU：设备地址 == 物理地址 ==
 *     虚拟地址）。 */

struct kcomp_block_device_api {
    /* 设备容量（单位：512 字节 sector）。 */
    uint64_t (*capacity_sectors)(void *ctx);
    /* 从 lba 读 len 字节到 buf。 */
    int32_t (*read)(void *ctx, uint64_t lba, uint8_t *buf, size_t len);
    /* 从 buf 写 len 字节到 lba。 */
    int32_t (*write)(void *ctx, uint64_t lba, const uint8_t *buf, size_t len);
};

_Static_assert(sizeof(struct kcomp_block_device_api) == 3 * sizeof(void *),
               "block.device 字段个数 / 顺序漂移（vs Rust BlockDeviceApi）");


#define KCOMP_FILESYSTEM_NAME "filesystem"
#define KCOMP_FILESYSTEM_ABI UINT64_C(0x46494C4553595354) /* ASCII "FILESYST" */

/* 第一阶段只读文件访问。flags 是 ABI 编码，不直接暴露 FatFs 的 FA_*。 */
#define KCOMP_FILESYSTEM_OPEN_READ UINT32_C(0x00000001)

/* Filesystem 的 function table */

struct kcomp_filesystem_api {

    int32_t (*mount)(void *ctx);

    int32_t (*unmount)(void *ctx);

    /* `path` 是以 NUL 结尾的、相对于该 filesystem root 的路径。 */
    int32_t (*open)(void *ctx, const char *path, uint32_t flags, uint64_t *out_handle);
    int32_t (*close)(void *ctx, uint64_t handle);

    int32_t (*read)(void *ctx, uint64_t handle, uint8_t *buf, size_t len, size_t *out_read);
};

_Static_assert(sizeof(struct kcomp_filesystem_api) == 5 * sizeof(void *),
               "filesystem api layout drift");

#ifdef __cplusplus
}
#endif

#endif /* KCOMP_H */
