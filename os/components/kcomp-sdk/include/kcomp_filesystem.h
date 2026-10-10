/* Typed IPC filesystem consumer facade; no transport branches or provider pointers. */
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

/* Validate an exact endpoint without granting send rights; composer grants separately. */
int32_t kcomp_filesystem_bind(uint64_t endpoint, uint64_t contract, uint64_t abi,
                              struct kcomp_filesystem_binding *out_binding);

/* 挂载 / 卸载该 filesystem 实例。成功（transport == 0）时 provider 的
 * `0 / -errno` 在 `result.method`。 */
struct kcomp_call_result kcomp_filesystem_mount(const struct kcomp_filesystem_binding *binding);
struct kcomp_call_result kcomp_filesystem_unmount(const struct kcomp_filesystem_binding *binding);
struct kcomp_call_result kcomp_filesystem_shutdown(const struct kcomp_filesystem_binding *binding);

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
