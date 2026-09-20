/* kcomp.h —— 组件 ABI 的 C 侧作者面（唯一权威声明）。
 *
 * 边界两半都写在这里：
 *   - `kcore_*` ：组件可调用的 Core 导出（EXPORT_SYMBOL 教学版白名单），名字与
 *                 签名必须与 Core `os/core/src/component/export.rs` 逐字节一致；
 *   - `kcomp_*` ：组件**必须导出**的生命周期入口与契约指纹，Core 在调用组件
 *                 代码前解析、校验（见 docs/component-lifecycle.md §4）。
 *
 * Rust 镜像在 `os/components/kcomp-sdk/src/abi.rs`；三方漂移由
 * `os/core/tests/kcomp_abi_drift.rs` 纯文本交叉校验。**改这里 = 改 ABI。**
 *
 * 约定（docs/component-lifecycle.md）：
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

#ifdef __cplusplus
extern "C" {
#endif

/* ===========================================================================
 * 稳定布局结构（与 Rust 镜像逐字段一致）
 * =========================================================================== */

/* 一条 trace 记录的稳定编码（Core `trace::abi::TraceRecordAbi`）。
 * `kind` 决定 a/b/c 的含义；缺失字段写 `UINT64_MAX`（不是 0）。 */
struct kcore_trace_record {
    uint64_t seq;
    uint64_t timestamp;
    uint32_t kind;
    uint32_t flags;
    uint64_t a;
    uint64_t b;
    uint64_t c;
};

/* 布局与指针宽度无关：RV64 / RV32 都是 48 字节、8 对齐。 */
_Static_assert(sizeof(struct kcore_trace_record) == 48,
               "kcore_trace_record layout drift vs Core trace::abi::TraceRecordAbi");
_Static_assert(_Alignof(struct kcore_trace_record) == 8,
               "kcore_trace_record alignment drift");

/* Trace 子系统状态的稳定编码（Core `trace::abi::TraceStatsAbi`）。
 * `overwritten_total` = 因 ring 满被逐出的记录总数，不是某个 reader 漏掉的条数。 */
struct kcore_trace_stats {
    uint64_t capacity;
    uint64_t oldest_seq;
    uint64_t next_seq;
    uint64_t overwritten_total;
    uint64_t enabled_mask;
};

_Static_assert(sizeof(struct kcore_trace_stats) == 40,
               "kcore_trace_stats layout drift vs Core trace::abi::TraceStatsAbi");
_Static_assert(_Alignof(struct kcore_trace_stats) == 8,
               "kcore_trace_stats alignment drift");

/* `kcomp_instance_create` 的参数：仅在调用期间借用。
 * `config` 必须拷贝后才能持久化；Core 视其为不透明字节。 */
struct KcompCreateArgs {
    uint64_t config_abi;   /* config 负载的精确指纹；0 = 无负载 */
    const void *config;    /* Core 视为不透明字节 */
    size_t config_len;
};

/* 指针宽度相关：RV64 = 8 + 8 + 8 = 24；RV32 = 8 + 4 + 4 = 16
 * （RISC-V ILP32 下 uint64_t 仍 8 字节对齐，config_len 紧跟指针、无额外 padding）。 */
#if __SIZEOF_POINTER__ == 8
_Static_assert(sizeof(struct KcompCreateArgs) == 24,
               "KcompCreateArgs layout drift on RV64 (host layout check in SDK tests.rs)");
#else
_Static_assert(sizeof(struct KcompCreateArgs) == 16,
               "KcompCreateArgs layout drift on RV32 (host layout check in SDK tests.rs)");
#endif
_Static_assert(_Alignof(struct KcompCreateArgs) == 8, "KcompCreateArgs alignment drift");
_Static_assert(offsetof(struct KcompCreateArgs, config) == 8, "KcompCreateArgs.config offset drift");
#if __SIZEOF_POINTER__ == 8
_Static_assert(offsetof(struct KcompCreateArgs, config_len) == 16,
               "KcompCreateArgs.config_len offset drift on RV64");
#else
_Static_assert(offsetof(struct KcompCreateArgs, config_len) == 12,
               "KcompCreateArgs.config_len offset drift on RV32");
#endif

/* ===========================================================================
 * 组件生命周期入口（组件**导出**，Core 调用）
 * =========================================================================== */

/* 精确契约指纹（手工维护，非版本号）。Core 在调用组件代码前校验其 ELF 定义、
 * 边界与值。组件必须定义 `const uint64_t kcomp_abi = ...;`——SDK 的
 * `kcomp_instance_create!` 宏会发出该定义（值 = Rust 镜像的 `KCOMP_ABI`）。 */
extern const uint64_t kcomp_abi;

/* 必需导出。返回 0 或负 errno。
 * Core 调用前把 *out_state 初始化为 NULL；成功时组件写入自己完成的 state
 * 指针；**无状态组件可以成功返回 NULL**。状态经共享堆分配，Core 只存/传指针。
 *
 * 失败 / panic → 现有 Failed 路径（revoke + discard pending）；panic 或未完整
 * 构造的实例**不会**被调用 destroy（清理是组件自己的责任）。 */
int32_t kcomp_instance_create(const struct KcompCreateArgs *args, void **out_state);
int32_t kcomp_instance_destroy(void *state);

/* Core 侧的最小创建操作：按 artifact 名创建新实例（同一 image 允许多实例）。
 * 成功 = 0 且 *out_instance 写 instance id（ComponentId raw）；失败 = -errno。
 * `args` 仅在调用期间借用。`kcore_component_load` 保留为默认配置便利操作。 */
int32_t kcore_component_create(const uint8_t *image_name, size_t image_name_len,
                               const struct KcompCreateArgs *args, uint32_t *out_instance);

/* ===========================================================================
 * 任务 ABI
 * =========================================================================== */

/* 任务入口：`arg` 由 `kcore_task_create` 原样回传。
 * 契约：entry 必须经 Core 退出（kcore_task_exit）；任务归属来自 Core 的执行
 * 边界，**不是**来自 `arg`。 */
typedef void (*KcompTaskEntry)(void *arg);

/* ===========================================================================
 * kcore_* —— Core 导出白名单（名字与 export.rs 精确一致）
 * =========================================================================== */

/* -- Runtime / shared heap（Core 共享堆，非 per-component 堆）-- */
uint8_t *kcore_heap_alloc(size_t size, size_t align);
int32_t kcore_heap_dealloc(uint8_t *ptr, size_t size, size_t align);

/* -- Logging / diagnostics -- */
void kcore_console_write_byte(uint8_t byte);
int32_t kcore_log_line(const uint8_t *ptr, size_t len);

/* -- Trace（只读观察面；无写入口）-- */
/* 读 `seq >= since` 的第一条记录；没有更多时返回 -ENOENT（不返回 0）。 */
int32_t kcore_trace_read(uint64_t since, struct kcore_trace_record *out, uint64_t *out_next);
int32_t kcore_trace_stats(struct kcore_trace_stats *out);

/* -- Clock（只读；无 authority 语义）-- */
uint64_t kcore_now(void);
uint64_t kcore_timebase_hz(void);

/* -- Machine query -- */
uint32_t kcore_machine_boot_hart(void);
uint32_t kcore_machine_cpu_count(void);
int32_t kcore_machine_has_hart(uint32_t hart_id);

/* -- System query -- */
uint32_t kcore_free_page_count(void);
uint32_t kcore_task_count(void);
uint32_t kcore_component_count(void);

/* -- Component lifecycle -- */
int32_t kcore_component_load(const uint8_t *name, size_t len);
int32_t kcore_interface_publish(const uint8_t *name, size_t len, uint32_t kind, uint64_t abi,
                                const void *api, void *ctx);
int32_t kcore_interface_available(const uint8_t *name, size_t len, uint32_t kind, uint64_t abi);
int32_t kcore_interface_bind(const uint8_t *name, size_t len, uint32_t kind, uint64_t abi,
                             uint64_t *out_binding, size_t *out_api, size_t *out_ctx,
                             uint64_t *out_generation);
int32_t kcore_interface_refresh(uint64_t binding, uint64_t abi, size_t *out_api, size_t *out_ctx,
                                uint64_t *out_generation);

/* -- Task control -- */
int32_t kcore_task_create(KcompTaskEntry entry, void *arg, uint32_t *out_task);
int32_t kcore_task_start(uint32_t id);
int32_t kcore_task_yield(void);
int32_t kcore_task_exit(void);
int32_t kcore_task_state(uint32_t id);

/* -- Panic containment -- */
int32_t kcore_panic_escape(void);

/* -- Scheduler -- */
int32_t kcore_sched_run(void);

/* -- Device ownership / MMIO（mechanism-first：claim 后直接拿 MMIO 指针）-- */
int32_t kcore_device_nth(const uint8_t *compatible, size_t len, uint32_t ordinal,
                         uint32_t *out_device_id);
/* 认领确切设备：Core 记 owner，返回本执行域下的 MMIO 指针 + 长度。
 * KernelNative 下就是寄存器基址；driver 之后自己 volatile 读写。 */
int32_t kcore_device_claim(uint32_t device_id, uint8_t **out_mmio, size_t *out_len);
int32_t kcore_device_release(uint32_t device_id);

/* -- IRQ routes（锚点是 DeviceId；只支持 native callback）-- */
int32_t kcore_irq_register(uint32_t device_id, void (*handler)(void *ctx), void *ctx);
int32_t kcore_irq_enable(uint32_t device_id);
int32_t kcore_irq_disable(uint32_t device_id);
int32_t kcore_irq_release(uint32_t device_id);

/* -- DMA（allocation 与 mapping 分离）-- */
int32_t kcore_dma_alloc(size_t size, uint8_t **out_ptr, size_t *out_len);
int32_t kcore_dma_free(uint8_t *ptr);
int32_t kcore_dma_map(uint32_t device_id, uint8_t *ptr, size_t len, int32_t direction,
                      uint64_t *out_device_addr, uint64_t *out_mapping);
int32_t kcore_dma_unmap(uint64_t mapping);

#ifdef __cplusplus
}
#endif

#endif /* KCOMP_H */
