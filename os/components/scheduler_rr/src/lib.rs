//! RR（轮转）调度器组件 —— SchedulerPolicy 的参考实现（M2/C4）。
//!
//! 策略只保存一个 RR cursor（runqueue 真相由 Core 每次调用时传入，本组件
//! 不持有）。`choose_next` 只做一件事：在 Core 给的 runnable 列表里轮流
//! 提议下一个 TaskId。**提议**是否被采纳由 Core 验证后决定——本组件永远
//! 拿不到任务表、状态或任何 Core truth 的写权限。
//!
//! 实例生命周期（`docs/architecture/component-lifecycle.md` §3/§10）：cursor 是**实例状态**，
//! 在 `kcomp_instance_create` 里经 Core 共享堆分配，并作为服务 `ctx` 交给 Core；
//! `choose_next` 经该 ctx 访问它。替换实例 = 全新分配 = 全新 cursor。
//!
//! 接口发布：create 期间经 `binding::publish_named::<SchedulerPolicy>` 发布
//! `scheduler`（staged：create 返回 0 后 Core 才原子提交）。provider 身份由 Core
//! 从 create 上下文解析（组件不自报 id）。消费方按名字 bind 并 exact-compare ABI
//! fingerprint；替换本组件 = 换 provider，consumer 无需重编译。

#![no_std]

// 组件私有 panic adapter（kcomp-sdk）：提供裸机 #[panic_handler] 与 binding wrapper。
use kcomp_sdk as _;

#[cfg(test)]
extern crate std;

use core::sync::atomic::{AtomicU32, Ordering};
use kcomp_sdk::abi;
use kcomp_sdk::binding::{self, SchedulerPolicy, SchedulerPolicyApi};
use kcomp_sdk::errno::Errno;

/// RR 调度器**实例状态**：cursor 指向 runnable 列表中的下一个槽位。
///
/// 经 Core 共享堆分配（地址稳定）、作为 `scheduler` 服务的 opaque `ctx` 发布；
/// function table 是不可变契约（所有实例共享），状态才随实例走。
/// [`rr_choose_next`] 从 ctx 解引用它——不同实例各自独立轮转。
#[repr(C)]
struct SchedulerState {
    cursor: AtomicU32,
}

/// RR 提议：在 Core 传入的 runnable（id 升序）里轮流选择。
///
/// runnable 列表每次由 Core 重新收集（yield 者不在自己看到的列表里），
/// cursor 对 `count` 取模即自然轮转；count 收缩时取模也保持有效选择。
extern "C" fn rr_choose_next(
    ctx: *mut (),
    runnable: *const u32,
    count: usize,
    _current: u32,
) -> u32 {
    if count == 0 {
        return u32::MAX; // 无任务：Core 不会为此调用我们；防御性返回。
    }
    // SAFETY: ctx 由本组件 create 发布（binding ctx），Core 只存取、原样回传；
    // 指向共享堆上实例存活期内有效的 SchedulerState。
    let state = unsafe { &*(ctx as *const SchedulerState) };
    let slot = state.cursor.fetch_add(1, Ordering::SeqCst) as usize % count;
    // SAFETY: runnable 由 Core 保证指向 count 个有效 u32（契约）。
    unsafe { *runnable.add(slot) }
}

/// 发布的 function table（发布后由 Core 保存 api 指针 + ctx）。
/// 不可变契约表保持 image-global：所有实例共享同一个 VTABLE（契约 §10）。
static VTABLE: SchedulerPolicyApi = SchedulerPolicyApi {
    choose_next: rr_choose_next,
};

// 实例创建入口（C ABI，契约 §4）：分配并初始化 per-instance state，发布
// SchedulerPolicy。
//
// `0` = 成功（`*out_state` = 本实例 state）；负 errno = 失败，Core 走 Failed 且
// **不会**调用 destroy（构造期清理由本入口负责）。config 不进状态：RR 无配置，
// 默认配置 = cursor 0。
kcomp_sdk::kcomp_instance_create!(|_args, out_state| {
    let size = core::mem::size_of::<SchedulerState>();
    let align = core::mem::align_of::<SchedulerState>();
    // SAFETY: 纯分配调用，无所有权语义；成功 = 对齐的 size 字节，失败 = NULL。
    let state = unsafe { abi::kcore_heap_alloc(size, align) };
    if state.is_null() {
        return Errno::ENOMEM.code();
    }
    // 新实例从 cursor 0 开始：替换实例拿到全新 cursor。
    // SAFETY: state 是刚分配、对齐满足、尚未初始化的 SchedulerState 存储。
    unsafe {
        core::ptr::write(
            state.cast::<SchedulerState>(),
            SchedulerState {
                cursor: AtomicU32::new(0),
            },
        );
    }

    // SAFETY: `VTABLE` 是 'static 的 #[repr(C)] function table；state 是实例
    // 存活期内地址稳定的 opaque state（Core 只存指针、不解引用）。
    let published = unsafe {
        binding::publish_named::<SchedulerPolicy>(
            binding::SCHEDULER_POLICY_NAME,
            &VTABLE,
            state.cast::<()>(),
        )
    };
    if published.is_err() {
        // 发布失败：pending 发布未提交，Core 不调用 destroy；构造期清理由组件
        // 自己负责——释放刚分配的 state。返回码保持旧入口的 -1（可观测行为不变）。
        // SAFETY: state 来自本次 create 的 kcore_heap_alloc（size/align 相同）。
        unsafe {
            abi::kcore_heap_dealloc(state, size, align);
        }
        return -1;
    }

    // Core 调用前把 *out_state 初始化为 NULL；成功时写回自己完成的 state 指针。
    // SAFETY: out_state 由 Core 保证可写（create 调用契约）。
    unsafe {
        *out_state = state.cast::<()>();
    }
    kcomp_sdk::klog!("scheduler_rr: policy published");
    0
});

// 实例析构入口：Core 停止路径（monitor `unload`）调用，返回 0 后才提交 Stopped；
// 失败 / panic → Core 置 Failed 且**绝不重试**。scheduler_rr 不持有
// MMIO/IRQ/DMA authority、不拥有任务；唯一的分配（cursor state）是**已发布出去的
// binding ctx**——契约 §8 本轮保留已暴露的 state 存储（consumer 可能持有拷贝过的
// binding，释放 ctx 会变成 use-after-free）。这里只留一行可观测证据。
kcomp_sdk::kcomp_instance_destroy!(|_state| {
    kcomp_sdk::klog!("scheduler_rr: instance destroyed");
    0
});

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个实例状态，返回它的 binding ctx（栈上，测试帧内有效）。
    ///
    /// 注意：不能把它装进返回值再取指针（move 会让指针悬空）；各测试在本地
    /// 绑定后立刻取址。
    fn instance_state(cursor: u32) -> SchedulerState {
        SchedulerState {
            cursor: AtomicU32::new(cursor),
        }
    }

    /// RR 轮转的纯逻辑：新实例 cursor=0，对 [0,1] 连续提议必须是 0 1 0 1。
    #[test]
    fn rr_alternates_over_runnable_list() {
        let runnable = [0u32, 1u32];
        let state = instance_state(0);
        let ctx = &state as *const SchedulerState as *mut ();
        let first = rr_choose_next(ctx, runnable.as_ptr(), 2, u32::MAX);
        let second = rr_choose_next(ctx, runnable.as_ptr(), 2, first);
        assert_eq!((first, second), (0, 1), "从 cursor 0 起严格交替");
        let third = rr_choose_next(ctx, runnable.as_ptr(), 2, second);
        assert_eq!(third, 0, "第三次回到首项（模 2 轮转）");
    }

    /// 收缩列表仍然给出有效提议（cursor 对 count 取模）。
    #[test]
    fn rr_stays_valid_when_list_shrinks() {
        let runnable = [7u32];
        let state = instance_state(0);
        let ctx = &state as *const SchedulerState as *mut ();
        for _ in 0..4 {
            let pick = rr_choose_next(ctx, runnable.as_ptr(), 1, 7);
            assert_eq!(pick, 7);
        }
    }

    /// cursor 是 per-instance 的：两个实例状态各自独立轮转，新实例从 0 开始
    /// （不再是 image-global 的共享 cursor）。
    #[test]
    fn rr_cursors_are_per_instance() {
        let runnable = [0u32, 1u32];
        let first = instance_state(0);
        let second = instance_state(0);
        let first_ctx = &first as *const SchedulerState as *mut ();
        let second_ctx = &second as *const SchedulerState as *mut ();
        assert_eq!(rr_choose_next(first_ctx, runnable.as_ptr(), 2, u32::MAX), 0);
        assert_eq!(rr_choose_next(first_ctx, runnable.as_ptr(), 2, 0), 1);
        assert_eq!(
            rr_choose_next(second_ctx, runnable.as_ptr(), 2, u32::MAX),
            0,
            "另一个实例不受影响，从自己的 cursor 0 开始"
        );
    }
}
