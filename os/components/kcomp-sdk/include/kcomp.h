/* kcomp.h —— 组件 ABI 的 C 侧作者面（umbrella；声明本体在 abi/ 的 schema）。
 *
 * 边界两半都由 tools/kabi/kabi_gen.py 从 schema 生成到
 * `generated/kcomp_abi.h`（本文件只 include 它 + 保留契约说明）：
 *   - `kcore_*` 导出 / 组件生命周期入口 / 稳定结构 / 常量 / 接口分类枚举：
 *     `abi/component.toml` + `abi/core.toml`；
 *   - `kcomp_*` 组件间契约（`block.device` / `filesystem` function table 与常量）：
 *     `abi/block.toml` + `abi/filesystem.toml`。
 * 生成物带布局 `_Static_assert`（C 侧）/ `const _`（Rust 侧）。
 *
 * Rust 镜像：`os/components/kcomp-sdk/src/abi.rs`（手写 facade）+
 * `src/generated/{abi,block,filesystem}.rs`（生成物）；Core 侧
 * `os/core/src/generated/abi.rs` + `os/core/src/component/generated/exports.rs`
 * （typed 导出注册表）。
 * **改 ABI = 改 abi/ 的 schema**，然后 `make abi-gen`（`make abi-check` 校验漂移）。
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

/* ===========================================================================
 * 组件间契约（C）—— endpoint 模型的 function table
 * ===========================================================================
 *
 * 这些**不是** Core 导出：Core 只把 publish 进来的 `api` / `ctx` 当不透明指针
 * 存着，不认识契约语义。所以名字是 `kcomp_*`（组件面），不是 `kcore_*`。
 * 机制本身用已有导出即可：provider 调 `kcore_endpoint_publish` 交付 function
 * table（Direct）与 dispatch token（Gate）；consumer 经 `kcore_endpoint_lookup`
 * 发现、`kcore_endpoint_bind` 拿 **Core 在 bind 时选定**的机制（Direct 交付
 * api/ctx；Gate 只给 opaque EndpointId，调用走 `kcore_endpoint_call`）。
 *
 * 声明本体（`struct kcomp_block_device_api` / `struct kcomp_filesystem_api` +
 * 名字 / 指纹 / sector / open-read 常量）在 `generated/kcomp_abi.h`；provider 与
 * consumer 编译**同一份**契约，Core 在 bind 时 exact-compare ABI 指纹。C ↔ Rust
 * 布局一致性由单源生成 + 生成物里的 `_Static_assert`（C）/ `const _`（Rust）
 * 编译器背书，不再靠文本交叉校验。
 */

/* Endpoint 调用路径的 C 包装（手写）：`kcomp_block_read` / `kcomp_filesystem_read`
 * 等，与 SDK-Rust typed 前端同线格式（`abi/block.toml` / `abi/filesystem.toml`
 * 单源常量）。公共结果类型 `struct kcomp_call_result` 在 `kcomp_call.h`。 */
#include "kcomp_call.h"
#include "kcomp_block.h"
#include "kcomp_filesystem.h"

/* raw backing 便利分配器（`kcore_memory_acquire/release` 的薄包装；与
 * `kcomp_kalloc.h` 的 per-instance 堆正交——普通 malloc/free 走后者）。 */
#include "kcomp_mem.h"

#endif /* KCOMP_H */
