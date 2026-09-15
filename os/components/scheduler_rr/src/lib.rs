//! RR（轮转）调度器组件 —— SchedulerPolicy 的参考实现（M2/C4）。
//!
//! 策略只保存一个 RR cursor（runqueue 真相由 Core 每次调用时传入，本组件
//! 不持有）。`choose_next` 只做一件事：在 Core 给的 runnable 列表里轮流
//! 提议下一个 TaskId。**提议**是否被采纳由 Core 验证后决定——本组件永远
//! 拿不到任务表、状态或任何 Core truth 的写权限。
//!
//! 接口发布：`kcomp_init` 里经 `kcomp_sdk::binding::publish("scheduler", Policy,
//! SCHEDULER_POLICY_ABI, &VTABLE, ctx)`；provider 身份由 Core 从 call_init 上下文
//! 解析（组件不自报 id）。发布是 **staged**：init 期间 Core 只记 pending，init
//! 返回 0 后才提交。消费方按名字 bind 并 exact-compare ABI fingerprint；替换本
//! 组件 = 换 provider，consumer 无需重编译。

#![no_std]

// 组件私有 panic adapter（kcomp-sdk）：提供裸机 #[panic_handler] 与 binding wrapper。
use kcomp_sdk as _;

#[cfg(test)]
extern crate std;

use core::sync::atomic::{AtomicU32, Ordering};

/// SchedulerPolicy function table（与 Core `sched::SchedulerPolicyApi` 布局一致——
/// A/B 双侧 ABI 契约；改动 = 破坏性变更，双侧同步）。`ctx` 由 Core 从 binding
/// 单独持有并回传，不在 table 内。
#[repr(C)]
pub struct SchedulerPolicyApi {
    pub choose_next:
        extern "C" fn(ctx: *mut (), runnable: *const u32, count: usize, current: u32) -> u32,
}

// function table 是只读契约，指针只被 Core 读取（provider 存活期内有效）。
unsafe impl Sync for SchedulerPolicyApi {}

/// RR cursor：指向 runnable 列表中的下一个槽位（跨调用保持，轮转推进）。
static CURSOR: AtomicU32 = AtomicU32::new(0);

/// RR 提议：在 Core 传入的 runnable（id 升序）里轮流选择。
///
/// runnable 列表每次由 Core 重新收集（yield 者不在自己看到的列表里），
/// cursor 对 `count` 取模即自然轮转；count 收缩时取模也保持有效选择。
extern "C" fn rr_choose_next(
    _ctx: *mut (),
    runnable: *const u32,
    count: usize,
    _current: u32,
) -> u32 {
    if count == 0 {
        return u32::MAX; // 无任务：Core 不会为此调用我们；防御性返回。
    }
    let slot = CURSOR.fetch_add(1, Ordering::SeqCst) as usize % count;
    // SAFETY: runnable 由 Core 保证指向 count 个有效 u32（契约）。
    unsafe { *runnable.add(slot) }
}

/// 发布的 function table（发布后由 Core 保存 api 指针 + ctx）。
static VTABLE: SchedulerPolicyApi = SchedulerPolicyApi {
    choose_next: rr_choose_next,
};

/// 组件入口（Linux module_init 约定）：发布 SchedulerPolicy。
/// 0 = 成功；非 0 = 发布失败（provider 状态/接口名冲突等由 Core 拒绝）。
#[unsafe(no_mangle)]
pub extern "C" fn kcomp_init() -> i32 {
    use kcomp_sdk::binding;
    // SAFETY: `VTABLE` 是 'static 的 #[repr(C)] function table；ctx 为 null
    // （RR 策略状态在组件静态 CURSOR 内，function table 无需 opaque state）。
    let published = unsafe {
        binding::publish(
            b"scheduler",
            binding::InterfaceKind::Policy,
            binding::SCHEDULER_POLICY_ABI,
            &VTABLE as *const SchedulerPolicyApi as *const (),
            core::ptr::null_mut(),
        )
    };
    if published.is_err() {
        return -1;
    }
    kcomp_sdk::klog!("scheduler_rr: policy published");
    0
}

// 退出钩子：Core 停止路径（monitor `unload`）会调用。scheduler_rr 不持有
// MMIO/IRQ/DMA authority（接口 teardown 由 Core 兜底解绑），显式 no-op。
kcomp_sdk::kcomp_exit!(0);

#[cfg(test)]
mod tests {
    use super::*;

    /// RR 轮转的纯逻辑：对 [0,1] 连续提议，顺序必须是 0 1 0 1。
    /// cursor 是静态的：跨测试共享，这里按相对顺序断言（首项 = 上次末尾的
    /// 下一项，取模后必然交替）。
    #[test]
    fn rr_alternates_over_runnable_list() {
        let runnable = [0u32, 1u32];
        let first = rr_choose_next(core::ptr::null_mut(), runnable.as_ptr(), 2, u32::MAX);
        let second = rr_choose_next(core::ptr::null_mut(), runnable.as_ptr(), 2, first);
        assert_ne!(first, second, "连续两次提议必须轮转");
        assert!(first < 2 && second < 2, "提议必须落在 runnable 列表内");
        let third = rr_choose_next(core::ptr::null_mut(), runnable.as_ptr(), 2, second);
        assert_eq!(third, first, "第三次回到首项（模 2 轮转）");
    }

    /// 收缩列表仍然给出有效提议（cursor 对 count 取模）。
    #[test]
    fn rr_stays_valid_when_list_shrinks() {
        let runnable = [7u32];
        for _ in 0..4 {
            let pick = rr_choose_next(core::ptr::null_mut(), runnable.as_ptr(), 1, 7);
            assert_eq!(pick, 7);
        }
    }
}
