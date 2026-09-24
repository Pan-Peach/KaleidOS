//! CoreTest 核内运行时：实例入口 + 报告分组编排。
//!
//! 入口契约见 `docs/architecture/component-lifecycle.md` §4：经 SDK 宏
//! `kcomp_instance_create!` / `kcomp_instance_destroy!` 导出（参数标识符由调用点
//! 给出，state 经 `*out_state` 交给 Core）。create 返回 `0`（core_test 的失败
//! 位图作为返回值下发）/ `-errno`；destroy 释放本实例的状态分配，失败返回
//! `-errno`（Core 置 Failed，绝不重试）。
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

use kcomp_sdk::errno::Errno;
use kcomp_sdk::mem;
use report::Checks;

// 实例创建入口（C ABI，`docs/architecture/component-lifecycle.md` §4）。
//
// 返回值与旧的 `kcomp_init` 完全一致：`0` = 全部通过；非 0 = 失败位图。
// 状态（`docs/architecture/component-lifecycle.md` §9）：`sched` 组的 A/B 迭代计数旧实现是
// `static mut`（image-global）；现在来自显式分配，指针经 `*out_state` 交 Core
// 保管。分配失败 → `-ENOMEM`，走 Core 的 Failed 路径（构造期清理由本组件负责，
// Core 不会调 destroy）。
kcomp_sdk::kcomp_instance_create!(|_args, out_state| {
    let size = core::mem::size_of::<sched::State>();
    let align = core::mem::align_of::<sched::State>();
    let region = match mem::mem_alloc(size as u64, align as u64) {
        Ok(view) => view,
        Err(_) => return Errno::ENOMEM.code(),
    };
    let state = region.base as *mut sched::State;
    // 逐字段初始化而非 `ptr::write`：不产生任何 memcpy/memset libcall，保持
    // freestanding（packer 只放行 `kcore_*` 未定义符号）。
    // SAFETY: state 指向 acquire 交付、已对齐的 State 存储。
    unsafe {
        (*state).a_count = 0;
        (*state).b_count = 0;
        (*state).region = region;
    }
    // SAFETY: out_state 由 Core 保证可写；Core 只存/传该指针，不解释、不释放。
    unsafe { *out_state = state.cast::<()>() };

    let mut checks = Checks::new();
    boot::group(&mut checks);
    let sched = sched::group(&mut checks, state);
    let resource = resource::group(&mut checks);
    trace::group(&mut checks, &sched, &resource);
    checks.finish()
});

// 实例析构入口（C ABI）：Core 停止路径（monitor `unload` / shutdown）调用。
//
// core_test 的 resource 用例全程自行 release authority（组结束时不剩常驻持有），
// 唯一持有的资源是本实例的 state 分配。先留一行可观测证据，再释放该分配；释放
// 失败返回 `-errno`（Core 置 Failed，绝不自动重试）。
kcomp_sdk::kcomp_instance_destroy!(|state| {
    for &c in "[core-test] teardown\n".as_bytes() {
        kcomp_sdk::console_write_byte(c);
    }
    // SAFETY: state 是本组件 create 经 `*out_state` 写回、Core 原样交还的同一分配；
    // region 是 acquire 交付的原样 view。
    let region = unsafe { (*state.cast::<sched::State>()).region };
    match mem::mem_release(region) {
        Ok(()) => 0,
        Err(error) => error.code(),
    }
});
