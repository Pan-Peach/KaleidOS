//! CoreTest 核内运行时：`kcomp_init` 入口 + 报告分组编排。
//!
//! 输出契约（`tests/qemu/runner.py` 按连续子串匹配，改动即破坏 CI）：
//! - 每项检查一行 `[core-test]   <name>: PASS|FAIL`；
//! - 汇总 `[core-test]   N/N checks PASS`；
//! - 终判 `[core-test] all: PASS`（颜色码只包在标记之外，实现点在 `report.rs`）。
//!
//! 分组顺序 = 依赖顺序：先 `boot` basics，再 `sched`（拿到 rr_id / task id），
//! 再 `resource`（拿到 handle），最后 `trace` 用这些 Core 返回值做
//! “这个操作产生这个事件”的精确断言。

mod boot;
mod report;
mod resource;
mod sched;
mod trace;

use report::Checks;

/// 组件入口（Linux module_init 约定）：0 = 全部通过；非 0 = 失败位图。
///
/// 位图由 [`Checks`] 维护（见 `report.rs`）；runner 只看返回值是否为 0。
#[unsafe(no_mangle)]
pub extern "C" fn kcomp_init() -> i32 {
    let mut checks = Checks::new();
    boot::group(&mut checks);
    let sched = sched::group(&mut checks);
    let resource = resource::group(&mut checks);
    trace::group(&mut checks, &sched, &resource);
    checks.finish()
}

// TODO(component-exit): 退出收尾（停 DMA / mask IRQ / 释放 authority）——Core 只解析、从不调用，当前显式 no-op。
kcomp_sdk::kcomp_exit!(0);
