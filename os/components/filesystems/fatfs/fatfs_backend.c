/* fatfs_backend.c —— FatFs 的**业务后端**（只读 FAT 文件系统语义）。
 *
 * Direct 的 `#[repr(C)]` function table（fatfs.c）与 Gate 的扁平 method switch
 * （fatfs_service.c）调用**同一份**实现；业务代码不感知部署。每个业务方法成功时打
 * 一行 `[fatfs] <op>`——QEMU runner 用它对照 Gate 入口日志做差分断言。
 */
#include "kcomp.h"
#include "fatfs_internal.h"
#include <errno.h>
#include <string.h>

static int32_t fatfs_result(FRESULT result)
{
    switch (result)
    {
    case FR_OK:
        return 0;

    case FR_NO_FILE:
    case FR_NO_PATH:
        return -ENOENT;

    case FR_DISK_ERR:
    case FR_INT_ERR:
        return -EIO;

    case FR_INVALID_OBJECT:
        return -EBADF;

    case FR_DENIED:
        return -EACCES;

    case FR_WRITE_PROTECTED:
        return -EROFS;

    case FR_NOT_ENOUGH_CORE:
        return -ENOMEM;

    case FR_EXIST:
        return -EEXIST;

    case FR_TIMEOUT:
        return -ETIMEDOUT;

    case FR_LOCKED:
        return -EBUSY;

    case FR_TOO_MANY_OPEN_FILES:
        return -EMFILE;

    case FR_INVALID_NAME:
    case FR_INVALID_PARAMETER:
        return -EINVAL;

    case FR_NOT_READY:
    case FR_INVALID_DRIVE:
    case FR_NOT_ENABLED:
    case FR_NO_FILESYSTEM:
        return -ENODEV;

    default:
        return -EIO;
    }
}

static struct fatfs_file_slot *fatfs_find(struct fatfs_state *state, uint64_t handle)
{
    if (handle != 0) {
        for (size_t i = 0; i < FATFS_MAX_OPEN_FILES; i++) {
            if (state->files[i].handle == handle)
                return &state->files[i];
        }
    }
    return NULL;
}

static int32_t fatfs_mount_locked(void *ctx)
{
    struct fatfs_state *state = ctx;

    if (state == NULL || !state->alive)
    {
        return -ENODEV;
    }

    if (state->mounted)
    {
        return 0; /* Already mounted */
    }

    if (state->last_node == UINT64_MAX)
        return -EOVERFLOW;

    FRESULT result = f_mount(&state->filesystem, "0:", 1);
    if (result != FR_OK)
    {
        return fatfs_result(result);
    }

    memset(state->nodes, 0, sizeof(state->nodes));
    state->nodes[0].id = ++state->last_node;
    state->nodes[0].kind = KCOMP_FILESYSTEM_NODE_DIRECTORY;
    strcpy(state->nodes[0].path, "0:");
    state->mounted = 1;
    FATFS_LOG_LINE("[fatfs] mount");
    return 0;
}

static int32_t fatfs_unmount_locked(void *ctx)
{
    struct fatfs_state *state = ctx;

    if (state == NULL || !state->alive)
    {
        return -ENODEV;
    }

    if (!state->mounted)
    {
        return 0; /* Already unmounted */
    }

    for (size_t i = 0; i < FATFS_MAX_OPEN_FILES; ++i)
    {
        if (state->files[i].handle)
        {
            return -EBUSY;
        }
    }

    FRESULT result = f_mount(NULL, "0:", 0);
    if (result != FR_OK)
    {
        return fatfs_result(result);
    }

    state->mounted = 0;
    memset(state->nodes, 0, sizeof(state->nodes));
    FATFS_LOG_LINE("[fatfs] unmount");
    return 0;
}

static int32_t fatfs_open_locked(void *ctx, const char *path, uint32_t flags, uint64_t *out_handle)
{
    struct fatfs_state *state = ctx;

    if (state == NULL || path == NULL || out_handle == NULL)
    {
        return -EINVAL;
    }

    *out_handle = 0;

    if (!state->alive)
        return -ENODEV;

    if (!state->mounted)
    {
        return -ENODEV;
    }

    if (flags != KCOMP_FILESYSTEM_OPEN_READ)
    {
        return -EROFS;
    }

    if (state->last_handle == UINT64_MAX)
        return -EOVERFLOW;

    // Find an available file slot
    int slot_index = -1;
    for (int i = 0; i < FATFS_MAX_OPEN_FILES; i++)
    {
        if (!state->files[i].handle)
        {
            slot_index = i;
            break;
        }
    }

    if (slot_index == -1)
    {
        return -EMFILE;
    }

    FIL *file = &state->files[slot_index].file;
    FRESULT result = f_open(file, path, FA_READ);
    if (result != FR_OK)
    {
        return fatfs_result(result);
    }

    *out_handle = ++state->last_handle;
    state->files[slot_index].handle = *out_handle;
    FATFS_LOG_LINE("[fatfs] open");

    return 0;
}

static int32_t fatfs_read_locked(
    void *ctx,
    uint64_t handle,
    uint8_t *buf,
    size_t len,
    size_t *out_read)
{
    struct fatfs_state *state = ctx;

    if (state == NULL || buf == NULL || out_read == NULL)
        return -EINVAL;

    *out_read = 0;

    if (!state->alive || !state->mounted)
        return -ENODEV;

    struct fatfs_file_slot *slot = fatfs_find(state, handle);
    if (slot == NULL)
        return -EBADF;

    if (len == 0)
        return 0;

    UINT request = len > (size_t)(UINT)-1
                       ? (UINT)-1
                       : (UINT)len;

    UINT actual = 0;
    FRESULT result = f_read(&slot->file, buf, request, &actual);
    if (result != FR_OK)
        return fatfs_result(result);

    *out_read = (size_t)actual;
    FATFS_LOG_LINE("[fatfs] read");
    return 0;
}

static int32_t fatfs_close_locked(void *ctx, uint64_t handle)
{
    struct fatfs_state *state = ctx;

    if (state == NULL)
        return -EBADF;

    if (!state->alive || !state->mounted)
        return -ENODEV;

    struct fatfs_file_slot *slot = fatfs_find(state, handle);
    if (slot == NULL)
        return -EBADF;

    FRESULT result = f_close(&slot->file);
    slot->handle = 0;
    slot->consumer = 0;
    slot->consumer_task = 0;
    if (result != FR_OK)
        return fatfs_result(result);
    FATFS_LOG_LINE("[fatfs] close");
    return 0;
}

int32_t fatfs_mount(void *ctx)
{
    struct fatfs_state *state = ctx;
    if (state == NULL)
        return -EINVAL;
    if (!fatfs_enter(state))
        return -EBUSY;
    int32_t result = fatfs_mount_locked(ctx);
    fatfs_leave(state);
    return result;
}

int32_t fatfs_unmount(void *ctx)
{
    struct fatfs_state *state = ctx;
    if (state == NULL)
        return -EINVAL;
    if (!fatfs_enter(state))
        return -EBUSY;
    int32_t result = fatfs_unmount_locked(ctx);
    fatfs_leave(state);
    return result;
}

int32_t fatfs_open(void *ctx, const char *path, uint32_t flags, uint64_t *out_handle)
{
    struct fatfs_state *state = ctx;
    if (state == NULL)
        return -EINVAL;
    if (!fatfs_enter(state))
        return -EBUSY;
    int32_t result = fatfs_open_locked(ctx, path, flags, out_handle);
    fatfs_leave(state);
    return result;
}

int32_t fatfs_close(void *ctx, uint64_t handle)
{
    struct fatfs_state *state = ctx;
    if (state == NULL)
        return -EINVAL;
    if (!fatfs_enter(state))
        return -EBUSY;
    int32_t result = fatfs_close_locked(ctx, handle);
    fatfs_leave(state);
    return result;
}

int32_t fatfs_read(void *ctx, uint64_t handle, uint8_t *buf, size_t len, size_t *out_read)
{
    struct fatfs_state *state = ctx;
    if (state == NULL)
        return -EINVAL;
    if (!fatfs_enter(state))
        return -EBUSY;
    int32_t result = fatfs_read_locked(ctx, handle, buf, len, out_read);
    fatfs_leave(state);
    return result;
}

static struct fatfs_node *fatfs_find_node(struct fatfs_state *state, uint64_t id)
{
    for (size_t i = 0; id != 0 && i < FATFS_MAX_NODES; ++i) {
        if (state->nodes[i].id == id)
            return &state->nodes[i];
    }
    return NULL;
}

/* 当前 FF_USE_LFN=0：只接受可精确表示的 ASCII 8.3 名字，避免 FatFs 截断、
 * 忽略尾点或空格后查到另一个名字。大小写匹配仍由 FatFs 完成。 */
static int32_t fatfs_check_name(const uint8_t *name, size_t len, uint32_t encoding)
{
    if (encoding != KCOMP_FILESYSTEM_ENCODING_BYTES)
        return -ENOTSUP;
    if (len == 0 || len > 12)
        return -EINVAL;
    size_t dot = len;
    for (size_t i = 0; i < len; ++i) {
        uint8_t ch = name[i];
        if (ch >= 0x80)
            return -ENOTSUP;
        if (ch <= 0x20 || ch == 0x7f || strchr("\"*+,/:;<=>?[\\]|", ch) != NULL)
            return -EINVAL;
        if (ch == '.') {
            if (dot != len)
                return -EINVAL;
            dot = i;
        }
    }
    if (dot == 0 || dot > 8 || (dot != len && (dot + 1 == len || len - dot - 1 > 3)))
        return -EINVAL;
    return 0;
}

static int32_t fatfs_lookup_locked(struct fatfs_state *state, uint64_t parent,
                                  const uint8_t *name, size_t name_len,
                                  uint32_t encoding, uint64_t *out_node)
{
    if (!state->alive || !state->mounted)
        return -ENODEV;
    struct fatfs_node *directory = fatfs_find_node(state, parent);
    if (directory == NULL)
        return -EBADF;
    if (directory->kind != KCOMP_FILESYSTEM_NODE_DIRECTORY)
        return -ENOTDIR;
    int32_t result = fatfs_check_name(name, name_len, encoding);
    if (result != 0)
        return result;

    char path[KCOMP_FILESYSTEM_PATH_MAX];
    size_t prefix = strlen(directory->path);
    if (prefix + 1 + name_len + 1 > sizeof(path))
        return -ENAMETOOLONG;
    memcpy(path, directory->path, prefix);
    path[prefix++] = '/';
    memcpy(path + prefix, name, name_len);
    path[prefix + name_len] = '\0';

    FILINFO info;
    FRESULT status = f_stat(path, &info);
    if (status != FR_OK)
        return fatfs_result(status);

    /* f_stat 的 fname 是磁盘目录项的原生名字，大小写别名共享同一 token。 */
    size_t canonical_len = strlen(info.fname);
    if (prefix + canonical_len + 1 > sizeof(path))
        return -ENAMETOOLONG;
    memcpy(path + prefix, info.fname, canonical_len + 1);
    struct fatfs_node *free_node = NULL;
    for (size_t i = 0; i < FATFS_MAX_NODES; ++i) {
        struct fatfs_node *node = &state->nodes[i];
        if (node->id != 0 && node->parent == parent &&
            strlen(node->path) == prefix + canonical_len &&
            memcmp(node->path, path, prefix + canonical_len) == 0) {
            *out_node = node->id;
            return 0;
        }
        if (node->id == 0 && free_node == NULL)
            free_node = node;
    }
    if (free_node == NULL)
        return -ENOSPC;
    if (state->last_node == UINT64_MAX)
        return -EOVERFLOW;
    strcpy(free_node->path, path);
    free_node->parent = parent;
    free_node->kind = (info.fattrib & AM_DIR) ? KCOMP_FILESYSTEM_NODE_DIRECTORY
                                             : KCOMP_FILESYSTEM_NODE_FILE;
    free_node->id = ++state->last_node;
    free_node->size = (uint64_t)info.fsize;
    *out_node = free_node->id;
    return 0;
}

int32_t fatfs_root(void *ctx, uint64_t *out_node)
{
    struct fatfs_state *state = ctx;
    if (state == NULL || out_node == NULL)
        return -EINVAL;
    *out_node = 0;
    if (!fatfs_enter(state))
        return -EBUSY;
    int32_t result = (!state->alive || !state->mounted) ? -ENODEV : 0;
    if (result == 0) {
        *out_node = state->nodes[0].id;
        FATFS_LOG_LINE("[fatfs] root");
    }
    fatfs_leave(state);
    return result;
}

int32_t fatfs_lookup(void *ctx, uint64_t parent, const uint8_t *name,
                     size_t name_len, uint32_t encoding, uint64_t *out_node)
{
    struct fatfs_state *state = ctx;
    if (state == NULL || name == NULL || out_node == NULL)
        return -EINVAL;
    *out_node = 0;
    if (!fatfs_enter(state))
        return -EBUSY;
    int32_t result = fatfs_lookup_locked(state, parent, name, name_len, encoding, out_node);
    if (result == 0)
        FATFS_LOG_LINE("[fatfs] lookup");
    fatfs_leave(state);
    return result;
}

int32_t fatfs_node_info(void *ctx, uint64_t id, uint32_t *out_kind)
{
    struct fatfs_state *state = ctx;
    if (state == NULL || out_kind == NULL)
        return -EINVAL;
    *out_kind = 0;
    if (!fatfs_enter(state))
        return -EBUSY;
    int32_t result = -ENODEV;
    if (state->alive && state->mounted) {
        struct fatfs_node *node = fatfs_find_node(state, id);
        result = node == NULL ? -EBADF : 0;
        if (node != NULL)
            *out_kind = node->kind;
    }
    if (result == 0)
        FATFS_LOG_LINE("[fatfs] node_info");
    fatfs_leave(state);
    return result;
}

int32_t kcomp_filesystem_wire_handle_node_details(void *ctx, uint64_t id,
    uint8_t *out, size_t output_len, struct kcomp_filesystem_wire_node_details_reply *reply)
{
    struct fatfs_state *state = ctx;
    if (!state || !out || output_len != 12 || !reply) return -EINVAL;
    if (!fatfs_enter(state)) return -EBUSY;
    struct fatfs_node *node = fatfs_find_node(state, id);
    int32_t result = (!state->alive || !state->mounted) ? -ENODEV : (node ? 0 : -EBADF);
    if (!result) {
        const char *name = node->path;
        for (const char *p = name; *p; ++p) if (*p == '/') name = p + 1;
        size_t len = node->parent ? strlen(name) : 0;
        if (len > 12) result = -EIO;
        else {
            reply->kind = node->kind;
            reply->name_length = (uint32_t)len;
            reply->size = node->size;
            memset(out, 0, output_len);
            memcpy(out, name, len);
        }
    }
    fatfs_leave(state);
    return result;
}

int32_t fatfs_open_node(void *ctx, uint64_t id, uint64_t *out_handle)
{
    struct fatfs_state *state = ctx;
    if (!state || !out_handle) return -EINVAL;
    if (!fatfs_enter(state)) return -EBUSY;
    struct fatfs_node *node = fatfs_find_node(state, id);
    int32_t result = !node ? -EBADF : node->kind != KCOMP_FILESYSTEM_NODE_FILE ? -EISDIR :
        fatfs_open_locked(state, node->path, KCOMP_FILESYSTEM_OPEN_READ, out_handle);
    fatfs_leave(state);
    return result;
}

int32_t fatfs_read_at(void *ctx, uint64_t handle, uint64_t offset,
                      uint8_t *buf, size_t len, size_t *out_read)
{
    struct fatfs_state *state = ctx;
    if (!state || !buf || !out_read) return -EINVAL;
    *out_read = 0;
    if (!fatfs_enter(state)) return -EBUSY;
    struct fatfs_file_slot *slot = fatfs_find(state, handle);
    int32_t result = (!state->alive || !state->mounted) ? -ENODEV : !slot ? -EBADF : 0;
    if (!result && offset > (uint64_t)(FSIZE_t)-1) result = -EOVERFLOW;
    if (!result) {
        FSIZE_t saved = f_tell(&slot->file);
        result = fatfs_result(f_lseek(&slot->file, (FSIZE_t)offset));
        if (!result) result = fatfs_read_locked(state, handle, buf, len, out_read);
        int32_t restored = fatfs_result(f_lseek(&slot->file, saved));
        if (!result) result = restored;
        if (result) *out_read = 0;
    }
    fatfs_leave(state);
    return result;
}
