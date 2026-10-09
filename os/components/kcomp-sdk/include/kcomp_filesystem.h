/* kcomp_filesystem.h —— filesystem 契约的 endpoint 调用 C 前端（consumer 侧）。
 *
 * 必须经 `kcomp.h` include（`kcore_endpoint_bind` / `KCOMP_FILESYSTEM_*` 的声明来自
 * `generated/kcomp_abi.h`）；不建议单独 include 本文件。
 *
 * 与 SDK-Rust 的 typed 前端（`src/filesystem/client.rs`）使用**同一条线格式**与同一个
 * 绑定形状：消费者只声明一个**不透明** `struct kcomp_filesystem_binding`，调
 * `kcomp_filesystem_bind` 取得它，再用统一的 `kcomp_filesystem_mount` /
 * `kcomp_filesystem_open` / `kcomp_filesystem_read` 调用。机制（Direct / Gate）由
 * Core 在 bind 时按 (caller, provider) 执行域选定，藏在绑定内部——消费者看不到、
 * 也不得解释它（**不暴露裸 function table**）。
 *
 * 线格式（`abi/filesystem.toml` 单源）：
 *   open(2)：args = 4 字节 LE `u32` flags；input = NUL 结尾路径（含结尾 NUL）
 *   close(3)：args = 8 字节 LE `u64` handle
 *   read(4)：args = 8 字节 LE `u64` handle；output = 8 字节 LE 实际长度 + 数据
 *
 * `read` 接受普通数据缓冲区（不含协议头）；成功时数据从 output 起，
 * *out_read 为实际长度。Gate 单次最多读 512 字节，允许正常短读。
 *
 * 实现（不透明绑定的内部表示）在 `kcomp-sdk/c/kcomp_filesystem.c`，随每个 C 组件
 * 私有携带；本头文件只暴露声明。
 */
#ifndef KCOMP_FILESYSTEM_H
#define KCOMP_FILESYSTEM_H

#include <stddef.h>
#include <stdint.h>

#include "kcomp_call.h"

/* 不透明调用绑定（内部表示）。组件只声明它、把它原样传回 `kcomp_filesystem_*`——
 * 字段是实现细节（Core 选定的机制藏在里面），不要在组件代码里解释。 */
struct kcomp_filesystem_binding {
    uint64_t opaque[4];
};

/* LE 编码：第 i 字节 = value >> (8*i)（不依赖宿主字节序，不做布局 cast）。 */
#define KCOMP_FILESYSTEM_U64_BYTE(value, i) \
    ((uint8_t)(((uint64_t)(value) >> (8u * (uint32_t)(i))) & 0xFFu))
#define KCOMP_FILESYSTEM_U32_BYTE(value, i) \
    ((uint8_t)(((uint32_t)(value) >> (8u * (uint32_t)(i))) & 0xFFu))

/* 编译期把 C 侧编码钉到与 Rust `filesystem::dispatch` 相同的固定向量上
 * （见 SDK 测试 `filesystem_wire_format_matches_the_c_wrapper_encoding`）：
 * 改任一侧的字节序 / 位序，这里编译失败。 */
_Static_assert(KCOMP_FILESYSTEM_U64_BYTE(0x0102030405060708ULL, 0) == 0x08u,
               "filesystem wire format drift (handle)");
_Static_assert(KCOMP_FILESYSTEM_U64_BYTE(0x0102030405060708ULL, 7) == 0x01u,
               "filesystem wire format drift (handle)");
_Static_assert(KCOMP_FILESYSTEM_U32_BYTE(0x01020304u, 0) == 0x04u,
               "filesystem wire format drift (flags)");
_Static_assert(KCOMP_FILESYSTEM_U32_BYTE(0x01020304u, 3) == 0x01u,
               "filesystem wire format drift (flags)");
_Static_assert(KCOMP_FILESYSTEM_HANDLE_LEN == 8u, "filesystem wire format drift (handle len)");
_Static_assert(KCOMP_FILESYSTEM_FLAGS_LEN == 4u, "filesystem wire format drift (flags len)");
_Static_assert(KCOMP_FILESYSTEM_READ_HEADER_LEN == 8u,
               "filesystem wire format drift (read header len)");
_Static_assert(KCOMP_FILESYSTEM_PATH_MAX == 256u, "filesystem wire format drift (path max)");

/* bind：调 Core（exact contract + abi + 存活校验 → 一次性选定机制）。
 *
 * `endpoint` 是组合策略经 create config 交给本组件的 opaque EndpointId（消费者
 * **不**做全局名字发现）；`contract` / `abi` 是本契约的编译期常量
 * （`KCOMP_FILESYSTEM_CONTRACT` / `KCOMP_FILESYSTEM_ABI`——两个不同的值）。
 * 成功 = 0（绑定可立即用于下面的调用）；失败 = -errno。 */
int32_t kcomp_filesystem_bind(uint64_t endpoint, uint64_t contract, uint64_t abi,
                              struct kcomp_filesystem_binding *out_binding);

/* 挂载 / 卸载该 filesystem 实例。成功（transport == 0）时 provider 的
 * `0 / -errno` 在 `result.method`。 */
struct kcomp_call_result kcomp_filesystem_mount(const struct kcomp_filesystem_binding *binding);
struct kcomp_call_result kcomp_filesystem_unmount(const struct kcomp_filesystem_binding *binding);

/* 打开 `path`（NUL 结尾、相对该 filesystem root；长度含结尾 NUL 必须
 * <= KCOMP_FILESYSTEM_PATH_MAX），handle 写回 `*out_handle`。 */
struct kcomp_call_result kcomp_filesystem_open(const struct kcomp_filesystem_binding *binding,
                                               const char *path, uint32_t flags,
                                               uint64_t *out_handle);

/* 关闭一个 open 返回的 handle。 */
struct kcomp_call_result kcomp_filesystem_close(const struct kcomp_filesystem_binding *binding,
                                                uint64_t handle);

/* 从当前位置读到普通数据缓冲区；成功时 *out_read 有效。 */
struct kcomp_call_result kcomp_filesystem_read(const struct kcomp_filesystem_binding *binding,
                                               uint64_t handle, void *output, size_t output_len,
                                               size_t *out_read);

/* 节点 token 与 open handle 分离；有效期见 abi/filesystem.toml。 */
struct kcomp_call_result kcomp_filesystem_root(const struct kcomp_filesystem_binding *binding,
                                               uint64_t *out_node);
struct kcomp_call_result kcomp_filesystem_lookup(const struct kcomp_filesystem_binding *binding,
                                                 uint64_t parent, const uint8_t *name,
                                                 size_t name_len, uint32_t encoding,
                                                 uint64_t *out_node);
struct kcomp_call_result kcomp_filesystem_node_info(const struct kcomp_filesystem_binding *binding,
                                                    uint64_t node, uint32_t *out_kind);

#endif /* KCOMP_FILESYSTEM_H */
