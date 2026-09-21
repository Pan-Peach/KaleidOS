/* kcomp_c_smoke —— 最小 C 组件：证明「C 前端 + Core 白名单」这条路端到端可用。
 *
 * 它**不走 Rust，也不自己写 extern 块**：只 `#include "kcomp.h"`，直接调
 * `kcore_*`（Core 导出白名单；组件唯一的名字空间）。C 运行时（mem*，见
 * kcomp-sdk/c/kcomp_rt.c）由 tools/build-kcomp-c.sh 自动随组件编入。
 *
 * 可观测行为（QEMU `test-c-smoke` 断言）：
 *   create  → `[c-smoke] hello from C`（经 kcore_log_line，证明白名单重定位 +
 *             返回值 `kcore_machine_cpu_count` 可调用）
 *   destroy → `[c-smoke] exit`         （Core 的卸载路径真的调用了 C 的析构入口）
 */
#include "kcomp.h"

/* 精确契约指纹（值与 Rust 侧 KCOMP_ABI 一致：8 字节 ASCII "KCOMPABI"）。
 * 必须定义，Core 在调用组件代码前校验其 ELF 定义、边界与值。 */
const uint64_t kcomp_abi = 0x4B434F4D50414249ULL;

int32_t kcomp_instance_create(const struct KcompCreateArgs *args, void **out_state) {
    (void)args;
    /* 无状态组件：成功返回且保持 Core 初始化的 NULL（见 docs/component-lifecycle.md）。 */
    *out_state = (void *)0;

    static const uint8_t hello[] = "[c-smoke] hello from C";
    kcore_log_line(hello, sizeof(hello) - 1);
    /* 额外的白名单往返：读一个 Core 导出的返回值（结果不作断言、只求可调用）。 */
    (void)kcore_machine_cpu_count();
    return 0;
}

int32_t kcomp_instance_destroy(void *state) {
    (void)state;
    static const uint8_t bye[] = "[c-smoke] exit";
    kcore_log_line(bye, sizeof(bye) - 1);
    return 0;
}
