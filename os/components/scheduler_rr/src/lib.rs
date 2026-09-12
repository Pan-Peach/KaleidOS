//! RR（轮转）调度器组件 —— SchedulerPolicy v1 的参考实现（M2/C4）。
//!
//! 策略只保存一个 RR cursor（runqueue 真相由 Core 每次调用时传入，本组件
//! 不持有）。`choose_next` 只做一件事：在 Core 给的 runnable 列表里轮流
//! 提议下一个 TaskId。**提议**是否被采纳由 Core 验证后决定——本组件永远
//! 拿不到任务表、状态或任何 Core truth 的写权限。
//!
//! 接口发布：`kcomp_init` 里 `kcore_interface_publish("scheduler", Policy, v1,
//! &VTABLE)`；provider 身份由 Core 从 call_init 上下文解析（组件不自报 id）。
//! 消费方（Core 调度 commit 路径）按名字 resolve，替换本组件 = 换 provider，
//! consumer 无需重编译。

#![no_std]

// 组件私有 panic adapter（kcomp-sdk）：只提供裸机 #[panic_handler]；本组件的
// 白名单 ABI 声明保留在此（未被引用的符号由 GC 丢弃）。
use kcomp_sdk as _;

#[cfg(test)]
extern crate std;

use core::sync::atomic::{AtomicU32, Ordering};

/// SchedulerPolicy v1 vtable（与 Core `sched::SchedulerPolicyV1` 布局一致——
/// A/B 双侧 ABI 契约；改动 = 破坏性变更，双侧同步 bump）。
#[repr(C)]
pub struct SchedulerPolicyV1 {
    pub version: u32,
    pub ctx: *mut (),
    pub choose_next:
        extern "C" fn(ctx: *mut (), runnable: *const u32, count: usize, current: u32) -> u32,
}

// vtable 是只读契约，指针只被 Core 读取（provider 存活期内有效）。
unsafe impl Sync for SchedulerPolicyV1 {}

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

/// 发布的 vtable（发布后由 Core 保存 context 指针）。
static VTABLE: SchedulerPolicyV1 = SchedulerPolicyV1 {
    version: 1,
    ctx: core::ptr::null_mut(),
    choose_next: rr_choose_next,
};

// 白名单 ABI（与 kernel `export.rs` 一一对应；C ABI 声明即契约）。
unsafe extern "C" {
    #[link_name = "kcore_interface_publish"]
    fn interface_publish(
        name: *const u8,
        len: usize,
        kind: u32,
        version: u32,
        context: *mut (),
    ) -> i32;
    #[link_name = "kcore_log_line"]
    fn log_line(ptr: *const u8, len: usize) -> i32;
}

/// InterfaceKind 的 ABI 编码（与 Core 一致）：2 = Policy。
const KIND_POLICY: u32 = 2;

/// 组件入口（Linux module_init 约定）：发布 SchedulerPolicy v1。
/// 0 = 成功；非 0 = 发布失败（provider 状态/接口名冲突等由 Core 拒绝）。
#[unsafe(no_mangle)]
pub extern "C" fn kcomp_init() -> i32 {
    let binding = unsafe {
        interface_publish(
            b"scheduler".as_ptr(),
            b"scheduler".len(),
            KIND_POLICY,
            1,
            &VTABLE as *const SchedulerPolicyV1 as *mut (),
        )
    };
    if binding < 0 {
        return -1;
    }
    const MSG: &[u8] = b"scheduler_rr: policy published";
    unsafe {
        log_line(MSG.as_ptr(), MSG.len());
    }
    0
}

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
