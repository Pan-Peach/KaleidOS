/* kcomp.h —— 组件 ABI 的 C 侧作者面（umbrella；声明本体在 abi/ 的 schema）。
 *
 * 边界两半都由 tools/kabi/kabi_gen.py 从 schema 生成到
 * `generated/kcomp_abi.h`（本文件只 include 它 + 保留契约说明）：
 *   - `kcore_*` 导出 / 组件生命周期入口 / 稳定结构 / 常量 / 接口分类枚举：
 *     `abi/component.toml` + `abi/core.toml`；
 *   - `kcomp_*` 组件间契约（`block.device` / `filesystem` 身份、值结构与常量）：
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

/* Ordinary component services use generated Endpoint Request/Reply protocols.
 * Providers retain state and objects in their own image; no Block/FS function tables.
 * Typed C facades preserve business sizes/errors while generated clients own wire bytes. */
#include "kcomp_call.h"
#include "kcomp_block.h"
#include "kcomp_filesystem.h"

/* raw backing 便利分配器（`kcore_memory_acquire/release` 的薄包装）。
 * KernelNative 的普通 malloc/free 走 `kcore_heap_alloc/dealloc`（共享 Core 堆，
 * 见 `generated/kcomp_abi.h`）；`kcomp_kalloc.h` 是未来私有执行域
 *（Isolated / Sandboxed）的私有分配器后端，与这条 backing 路径正交。 */
#include "kcomp_mem.h"

#endif /* KCOMP_H */
