//! CoreTest 核内运行时：实例入口 + 报告分组编排 + 组件/系统集成场景。
//!
//! 入口契约见 `docs/architecture/component-lifecycle.md` §4：经 SDK 宏
//! `kcomp_instance_create!` / `kcomp_instance_destroy!` 导出，state 经 `*out_state`
//! 交给 Core。create 全部通过返回 `0`，有检查失败返回**失败位图**（非 0；`load`
//! 命令据此报告 FAILED）/ `-errno`；destroy 释放本实例的状态分配，失败返回
//! `-errno`（Core 置 Failed，绝不重试）。
//!
//! 输出契约（`tests/qemu/runner.py` 按连续子串匹配，改动即破坏 CI）：
//! - 每项检查一行 `[core-test]   <name>: PASS|FAIL`；
//! - 汇总 `[core-test]   N/N checks PASS`；
//! - 终判 `[core-test] all: PASS`（颜色码只包在标记之外，实现点在 `report.rs`）。
//!
//! 分组顺序 = 依赖顺序：`boot` basics → `sched`（拿到 rr_id / task id）→
//! `resource`（拿到 handle）→ `trace` 用这些 Core 返回值做"这个操作产生这个事件"
//! 的精确断言；组件 / 系统集成场景随后执行。调度只能从 create 锚点上下文发起
//! （`kcore_sched_run` 的契约上下文），因此场景结果先写进 [`State`] 的对应字段，
//! 调度返回后再统一发报告行。

mod boot;
mod c_frontend;
mod driver;
mod filesystem;
mod report;
mod resource;
mod sched;
mod trace;

use kcomp_sdk::abi::{MemoryView, kcore_sched_run};
use kcomp_sdk::errno::Errno;
use kcomp_sdk::mem;
use report::Checks;

/// 本实例的可变状态：显式分配承载报告分组状态 + 场景结果 + backing 窗口。
///
/// 旧实现用 `static mut`（image-global）；按
/// `docs/architecture/component-lifecycle.md` §9，共享地址空间下 per-instance
/// 状态必须来自显式分配。指针经 `*out_state` 交 Core 保管，任务经
/// `kcore_task_create` 的 `arg` 拿回同一份。KernelNative 单 CPU，无并发。
#[repr(C)]
pub struct State {
    sched: sched::State,
    filesystem: filesystem::State,
    driver: driver::State,
    region: MemoryView,
}

/// 把 CPU 交给调度器（跑完所有 Runnable 任务后返回）。
fn schedule() {
    // SAFETY: 无指针参数；锚点上下文（create）调用是契约用法。
    if unsafe { kcore_sched_run() } != 0 {
        kcomp_sdk::klog!("[core-test] scenario scheduling failed");
    }
}

// 实例创建入口（C ABI，`docs/architecture/component-lifecycle.md` §4）。
//
// 返回值与旧的 `kcomp_init` 完全一致：`0` = 全部通过；非 0 = 失败位图。
kcomp_sdk::kcomp_instance_create!(|_args, out_state| {
    let size = core::mem::size_of::<State>();
    let align = core::mem::align_of::<State>();
    let region = match mem::mem_alloc(size as u64, align as u64) {
        Ok(view) => view,
        Err(_) => return Errno::ENOMEM.code(),
    };
    let state = region.base as *mut State;
    // 逐字段初始化而非 `ptr::write`：不产生任何 memcpy/memset libcall，保持
    // freestanding（packer 只放行 `kcore_*` 未定义符号）。
    // SAFETY: state 指向 acquire 交付、已对齐的 State 存储。
    unsafe {
        (*state).sched.a_count = 0;
        (*state).sched.b_count = 0;
        (*state).filesystem.block_chain = false;
        (*state).filesystem.block_chain_direct = false;
        (*state).filesystem.littlefs_multi = false;
        (*state).filesystem.littlefs_isolation = false;
        (*state).filesystem.littlefs_direct = false;
        (*state).filesystem.component_multi_instance = false;
        (*state).driver.prober_id = -1;
        (*state).driver.cursor = 0;
        (*state).driver.candidate_count = 0;
        (*state).driver.blk_mask = 0;
        (*state).driver.first_blk = u32::MAX;
        (*state).region = region;
    }
    // SAFETY: out_state 由 Core 保证可写；Core 只存/传该指针，不解释、不释放。
    unsafe { *out_state = state.cast::<()>() };

    let mut checks = Checks::new();
    boot::group(&mut checks);
    let sched = sched::group(&mut checks, unsafe {
        core::ptr::addr_of_mut!((*state).sched)
    });
    let resource = resource::group(&mut checks);
    trace::group(&mut checks, &sched, &resource);

    // 文件系统场景：task context 里跑（块/文件调用契约要求 task）。
    filesystem::spawn(unsafe { core::ptr::addr_of_mut!((*state).filesystem) });
    schedule();
    // SAFETY: 调度已返回，场景 task 退出；结果字段自此只读。
    let filesystem_state = unsafe { &*core::ptr::addr_of!((*state).filesystem) };
    filesystem::report(&mut checks, filesystem_state);

    // 驱动场景：真实 driver_prober 在 create 里建 dispatch 任务，第二轮调度跑它。
    // SAFETY: state 有效；prepare 内部不发起调度、不创建别名。
    let driver_state = unsafe { &mut *core::ptr::addr_of_mut!((*state).driver) };
    driver::prepare(&mut checks, driver_state);
    schedule();
    // SAFETY: 调度已返回，prober 的 dispatch 任务退出；此后只读。
    let driver_state = unsafe { &*core::ptr::addr_of!((*state).driver) };
    driver::report(&mut checks, driver_state);

    c_frontend::run(&mut checks);
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
    let region = unsafe { (*state.cast::<State>()).region };
    match mem::mem_release(region) {
        Ok(()) => 0,
        Err(error) => error.code(),
    }
});
