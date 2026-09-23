//! RR（轮转）调度器组件 —— `scheduler.policy` 的参考实现（M2/C4；step 5 迁移到
//! endpoint 模型）。
//!
//! 策略只保存一个 RR cursor（runqueue 真相由 Core 每次调用时传入，本组件
//! 不持有）。`CHOOSE_NEXT` 只做一件事：在 Core 给的 runnable 列表里轮流
//! 提议下一个 TaskId。**提议**是否被采纳由 Core 验证后决定——本组件永远
//! 拿不到任务表、状态或任何 Core truth 的写权限。
//!
//! 实例生命周期（`docs/architecture/component-lifecycle.md` §3/§10）：cursor 是**实例状态**，
//! 在 `kcomp_instance_create` 里经 Core 共享堆分配，`*out_state` 交 Core 保管；
//! Core 的 PolicyCall 边界把该 `instance_state` 交给本 image 的
//! `kcomp_service_dispatch`。替换实例 = 全新分配 = 全新 cursor。
//!
//! # 发布 / 消费（endpoint 模型，step 5）
//!
//! create 期间经 [`scheduler::publish_endpoint`] 发布 `scheduler.policy`
//! **Gate-only** endpoint（staged：create 返回 0 后 Core 才原子提交）：
//! 没有共享 function table、没有全局名字——策略执行只经 image 级
//! `kcomp_service_dispatch`（[`kcomp_sdk::kcomp_services!`]）。
//! 组合方（core_test / kbench / monitor）在 create 返回 0 之后**显式**发现该
//! endpoint 并 `kcore_sched_set_policy`；Core 的调度路径不按名字发现。

#![no_std]

// 组件私有 panic adapter（kcomp-sdk）：提供裸机 #[panic_handler] 与 binding wrapper。
use kcomp_sdk as _;

#[cfg(test)]
extern crate std;

use core::sync::atomic::{AtomicU32, Ordering};
use kcomp_sdk::abi;
use kcomp_sdk::errno::Errno;
use kcomp_sdk::frame;
use kcomp_sdk::scheduler::{self, SCHEDULER_POLICY_NAME};

/// provider 定义的不透明 dispatch token：image 级 `kcomp_service_dispatch` 用它
/// 选中 `scheduler.policy` 契约。单端口组件用 0。
pub const SCHEDULER_POLICY_PORT: u32 = 0;

/// RR 调度器**实例状态**：cursor 指向 runnable 列表中的下一个槽位。
///
/// 经 Core 共享堆分配（地址稳定）、作为实例 state 交给 Core；不同实例各自独立
/// 轮转（不再是 image-global 的共享 cursor）。
#[repr(C)]
struct SchedulerState {
    cursor: AtomicU32,
}

/// RR 提议：在 Core 传入的 runnable（id 升序）里轮流选择。
///
/// **算法与 step 5 前一致**：cursor 每次 +1，对 `count` 取模；runnable 列表每次
/// 由 Core 重新收集（yield 者不在自己看到的列表里），count 收缩时取模也保持
/// 有效选择。
fn rr_slot(state: &SchedulerState, count: usize) -> usize {
    if count == 0 {
        return 0; // 不可达：Core 的 frame 契约保证 input 非空；防御性返回。
    }
    state.cursor.fetch_add(1, Ordering::SeqCst) as usize % count
}

/// `CHOOSE_NEXT` 的处理：解码 frame → RR 选择 → 把提议写进 output。
///
/// 返回 `0` / `-errno`（方法状态）；Core 会验证提议落在 runnable 列表内，
/// 不在 → 提议被拒绝、本 provider 被隔离（逻辑死亡）。
fn choose_next(state: &SchedulerState, method: u32, call: frame::Call<'_>) -> i32 {
    if method != scheduler::SCHEDULER_METHOD_CHOOSE_NEXT {
        return Errno::ENOSYS.code();
    }
    let Some(request) = scheduler::ChooseNextRequest::decode(call.args, call.input) else {
        return Errno::EINVAL.code();
    };
    let slot = rr_slot(state, request.runnable_count());
    let Some(proposed) = request.runnable_at(slot) else {
        return Errno::EINVAL.code(); // 不可达：slot < runnable_count。
    };
    match scheduler::write_proposal(call.output, proposed) {
        Ok(()) => 0,
        Err(errno) => errno.code(),
    }
}

// image 级服务入口：port switch（只做 port → handler 路由；method switch 在
// handler 里，宏不发明协议）。
kcomp_sdk::kcomp_services! {
    state: SchedulerState;
    SCHEDULER_POLICY_PORT => choose_next,
}

// 实例创建入口（C ABI，契约 §4）：分配并初始化 per-instance state，发布
// `scheduler.policy` endpoint。
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

    // 发布策略 endpoint（Gate-only：api / ctx 为空；staged，create 返回 0 后
    // Core 原子提交）。发布失败：pending 未提交，Core 不调用 destroy；构造期
    // 清理由组件自己负责——释放刚分配的 state。返回码保持旧入口的 -1。
    if scheduler::publish_endpoint(SCHEDULER_POLICY_NAME, SCHEDULER_POLICY_PORT).is_err() {
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
    kcomp_sdk::klog!("scheduler_rr: policy endpoint published");
    0
});

// 实例析构入口：Core 停止路径（monitor `unload`）调用，返回 0 后才提交 Stopped；
// 失败 / panic → Core 置 Failed 且**绝不重试**。scheduler_rr 不持有
// MMIO/IRQ/DMA authority、不拥有任务；唯一的分配（cursor state）是**实例 state**——
// 契约 §8 本轮保留已暴露的 state 存储（Core / 调度帧可能仍持有它的拷贝）。
// 这里只留一行可观测证据。
kcomp_sdk::kcomp_instance_destroy!(|_state| {
    kcomp_sdk::klog!("scheduler_rr: instance destroyed");
    0
});

#[cfg(test)]
mod tests {
    use super::*;
    use kcomp_sdk::abi::KcompCallFrame;
    use kcomp_sdk::scheduler::{
        SCHEDULER_METHOD_CHOOSE_NEXT, SCHEDULER_NONE, SCHEDULER_TASK_ID_LEN,
    };

    fn instance_state(cursor: u32) -> SchedulerState {
        SchedulerState {
            cursor: AtomicU32::new(cursor),
        }
    }

    /// RR 轮转的纯逻辑：新实例 cursor=0，对 count=2 连续提议必须是 0 1 0 1。
    #[test]
    fn rr_alternates_over_runnable_list() {
        let state = instance_state(0);
        assert_eq!(rr_slot(&state, 2), 0, "从 cursor 0 起严格交替");
        assert_eq!(rr_slot(&state, 2), 1);
        assert_eq!(rr_slot(&state, 2), 0, "第三次回到首项（模 2 轮转）");
    }

    /// 收缩列表仍然给出有效槽位（cursor 对 count 取模）。
    #[test]
    fn rr_stays_valid_when_list_shrinks() {
        let state = instance_state(0);
        for _ in 0..4 {
            assert_eq!(rr_slot(&state, 1), 0);
        }
    }

    /// cursor 是 per-instance 的：两个实例状态各自独立轮转，新实例从 0 开始。
    #[test]
    fn rr_cursors_are_per_instance() {
        let first = instance_state(0);
        let second = instance_state(0);
        assert_eq!(rr_slot(&first, 2), 0);
        assert_eq!(rr_slot(&first, 2), 1);
        assert_eq!(rr_slot(&second, 2), 0, "另一个实例从自己的 cursor 0 开始");
    }

    /// 端到端 wire：CHOOSE_NEXT 解码 → RR 槽位 → 写 output。
    #[test]
    fn choose_next_writes_the_cursor_slot_proposal() {
        let state = instance_state(0);
        let args = SCHEDULER_NONE.to_le_bytes();
        let input = [3u32.to_le_bytes(), 5u32.to_le_bytes()].concat();
        let mut output = [0u8; SCHEDULER_TASK_ID_LEN];
        let frame = KcompCallFrame {
            args: args.as_ptr(),
            args_len: args.len(),
            input: input.as_ptr(),
            input_len: input.len(),
            output: output.as_mut_ptr(),
            output_len: output.len(),
        };

        // SAFETY: frame 与本帧内三个数组一致；闭包不逃逸借用。
        let status = unsafe {
            frame::with_call(&frame, |call| {
                choose_next(&state, SCHEDULER_METHOD_CHOOSE_NEXT, call)
            })
        };
        assert_eq!(status, Ok(0));
        assert_eq!(u32::from_le_bytes(output), 3, "第一次提议首项");

        // 第二次：cursor=1 → 提议次项。
        let status = unsafe {
            frame::with_call(&frame, |call| {
                choose_next(&state, SCHEDULER_METHOD_CHOOSE_NEXT, call)
            })
        };
        assert_eq!(status, Ok(0));
        assert_eq!(u32::from_le_bytes(output), 5);
    }

    /// 未知 method → ENOSYS；结构非法的 frame → EINVAL（都不写 output）。
    #[test]
    fn choose_next_rejects_unknown_method_and_malformed_frames() {
        let state = instance_state(0);
        let args = SCHEDULER_NONE.to_le_bytes();
        let input = 3u32.to_le_bytes();
        let mut output = [0u8; SCHEDULER_TASK_ID_LEN];
        let frame = KcompCallFrame {
            args: args.as_ptr(),
            args_len: args.len(),
            input: input.as_ptr(),
            input_len: input.len(),
            output: output.as_mut_ptr(),
            output_len: output.len(),
        };

        // SAFETY: frame 有效；闭包不逃逸借用。
        let status = unsafe { frame::with_call(&frame, |call| choose_next(&state, 7, call)) };
        assert_eq!(status, Ok(Errno::ENOSYS.code()));

        // 空 input（Core 契约保证非空）→ EINVAL。
        let empty = KcompCallFrame {
            args: args.as_ptr(),
            args_len: args.len(),
            input: core::ptr::null(),
            input_len: 0,
            output: output.as_mut_ptr(),
            output_len: output.len(),
        };
        // SAFETY: 空 input 长度 0，适配器不触碰指针。
        let status = unsafe {
            frame::with_call(&empty, |call| {
                choose_next(&state, SCHEDULER_METHOD_CHOOSE_NEXT, call)
            })
        };
        assert_eq!(status, Ok(Errno::EINVAL.code()));
    }
}
