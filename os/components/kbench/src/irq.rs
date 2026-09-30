//! `irq.uart_trigger_to_handler` —— owned UART 的自触发中断 → 组件 handler 入口。
//!
//! # 合法性边界（为什么这不是 benchmark 特权）
//!
//! 全程只走正常 device 链：`device_nth`（发现）→ `device_claim`（认领确切设备，
//! 直接拿 MMIO 裸指针）→ `irq_register`（锚在 DeviceId）→ `irq_enable`（开线）。
//! 触发是写**自己设备的** IER/THR 寄存器（ns16550a 的 TX-empty 中断；QEMU virt
//! 的 16550 没有 `reg-shift`，寄存器按字节编址：THR@0x00、IER@0x01），handler 用
//! 同一 owned device 的 **claim 指针**清 source。没有 raw PLIC 访问、没有全局
//! 中断控制、没有 benchmark god-mode、不关中断。
//!
//! 每一步失败都如实报告 `status=`（`error=` / `lease_len=` 给证据）：
//! `no_device` / `device_not_owned`（设备已被其它组件持有）/ `register_failed` /
//! `mmio_window_too_small`（claim 成功，但设备窗口不覆盖要碰的寄存器，带
//! `lease_len=`）/ `enable_failed` / `trigger_timeout`（触发路径不可用，达到丢弃
//! 上限）。kbench 不索取任何"只为 benchmark"的权限。
//!
//! # 测的是什么（必须连同数字一起读）
//!
//! 单次样本 = **触发写之前（`rdtime`）→ handler 入口（`rdtime`）** 的区间：
//!
//! - 区间**不是**"中断投递延迟"：它包含触发写自身的 lease MMIO 写（KernelNative
//!   快路径）、UART 设备模型、PLIC 网关、CPU trap 入口、PLIC claim、Core
//!   `route`、以及（trace 打开时）`IrqEnter` 的 emit 成本——**第一个事件的记录
//!   工作就在被测量区间内**；
//! - 已有 `IrqEnter`/`IrqAck` 事件**不能**这样用：`IrqEnter` 在 PLIC claim
//!   **之后**、`IrqAck` 在控制器 complete **之前**，那个区间是"claim 后 →
//!   complete 前"的插桩软件区间，不是投递延迟；本 primitive 不派生自它；
//! - QEMU TCG 不是 cycle-accurate：只作同环境相对信息，不做真机预测。
//!
//! # 采样纪律
//!
//! - 一次触发 = 一个样本（`method=one_shot`），不是 batch；端点各一次 `rdtime`，
//!   `min/median/p95/max/total` 全是原始 tick；
//! - warmup 样本（固定数量）不计入；超时（handler 未到）与倒退（entry < start）
//!   的样本**丢弃并计数**（`discarded_timeouts` / `discarded_backwards`），
//!   绝不把缺口拼成成功；
//! - 超时有界（`ticks_per_millisecond(hz) * 20`，不除法）；丢弃样本（超时 + 倒退）
//!   有硬上限，触发路径不可用时终止并报 `status=trigger_timeout`（绝不无界自旋）；
//! - baseline=none：IRQ 触发没有等价 null baseline（制造一个就等于关中断/抑制
//!   触发，被明令禁止），因此不做任何减法。

use core::sync::atomic::Ordering;

use kcomp_sdk::abi;

use crate::{Context, State, measure, report, stats};

const NAME: &str = "irq.uart_trigger_to_handler";
const UART_COMPATIBLE: &[u8] = b"ns16550a";
/// UART 寄存器**字节偏移**（8250/16550 兼容布局）。QEMU virt 的 ns16550a 没有
/// `reg-shift`（`serial_mm_init(..., regshift=0, ...)`），寄存器按字节编址：
/// THR@0x00、IER@0x01。这不是 u32 下标；Core 的 `kcore_mmio_write_u32` 也表达
/// 不了这个非 4 字节对齐的偏移，所以 IER 只经 lease 的字节写访问。
const REG_RBR_THR: usize = 0x00;
const REG_IER: usize = 0x01;
const IER_THRE: u8 = 0x02;
/// 本 primitive 触碰的最高寄存器字节（IER）+ 1。FDT 给 ns16550a 的窗口是 0x100
/// （`tests/fixtures/fdt/qemu-virt.dts`），不是一整页——曾把"需要一整页"当成
/// UART 的属性，把 Core 已经派生成功的 lease 本地误判为 `lease_failed`。
const MIN_HOST_LEN: usize = REG_IER + 1;
/// 编译期不变式（比运行期测试更早、更强）：寄存器字节偏移相邻，长度要求覆盖
/// IER 的最后一个字节，且不超出 QEMU virt 给 ns16550a 的 FDT 窗口（0x100）。
const _: () = {
    assert!(REG_RBR_THR == 0);
    assert!(REG_IER == REG_RBR_THR + 1);
    assert!(MIN_HOST_LEN > REG_IER);
    assert!(MIN_HOST_LEN <= 0x100);
};
/// 不计入统计的 warmup 样本数（固定数量，不做除法）。
const WARMUP_SAMPLES: u64 = 4;
/// 连续超时上限：超过即认为触发路径不可用，终止（有界，不挂死）。
const MAX_TIMEOUTS: u64 = 8;
/// 全部丢弃样本（超时 + 倒退）的硬上限：任何异常都不得变成无界自旋。
const MAX_DISCARDS: u64 = 64;
/// 单次等待的超时毫秒数（换算用二进制搜索，不做除法）。
const TIMEOUT_MS: u64 = 20;

// handler 的入口戳 / SERVED 标记 / lease 基址已迁入 `crate::State` 的
// `irq_entry_low`/`irq_served`/`irq_uart_lease` 字段（docs/architecture/component-lifecycle.md
// §10）。handler 经 `kcore_irq_register` 的 `ctx` 拿到 state 指针读取它们——
// 不再读 image-global static。区间远小于半程 2^31 tick，低位 wrapping 差值正确
// （见 [`classify`]）；RV32/RV64 都只有 32 位原子，入口戳仍存低 32 位。

/// 组件侧 handler（trap 上下文调用，锁外）。
///
/// 第一件事读入口时间戳（这就是测量端点）；随后把 owned device 的 IER 清零
/// （清 source，避免电平触发在后端 claim 循环里反复进入）。
/// 同一 trap 里若重复进入，只认第一次的时间戳（`SERVED` 先到先得）。
extern "C" fn handler(ctx: *mut ()) {
    let state = ctx as *mut State;
    // 先落入口戳、再置 SERVED（单 hart、trap 内关中断，观测者不会看到中间态；
    // 同一 trap 的重复 claim 不覆盖第一次的时间戳）。
    if !unsafe { (*state).irq_served.load(Ordering::SeqCst) } {
        let now = unsafe { abi::kcore_now() } as u32;
        unsafe { (*state).irq_entry_low.store(now, Ordering::SeqCst) };
        unsafe { (*state).irq_served.store(true, Ordering::SeqCst) };
    }
    let ptr = unsafe { (*state).irq_uart_lease.load(Ordering::SeqCst) } as *mut u8;
    if !ptr.is_null() {
        // SAFETY: lease (ptr, len >= MIN_HOST_LEN) 在本实验期间有效；offset 1 = IER
        //（QEMU virt 的 16550 字节编址）。volatile 写 owned device 的寄存器
        //（KernelNative fast path），不碰 PLIC。
        unsafe { core::ptr::write_volatile(ptr.add(REG_IER), 0) };
    }
}

/// 单次样本的分类（纯逻辑，host-testable）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sample {
    /// 有效区间（原始 tick）。
    Interval(u64),
    /// handler 从未到达（入口戳未更新）。
    Missing,
    /// 时间戳倒退（越过半程）：不做饱和减法，丢弃。
    Backwards,
}

/// entry / start **低 32 位**的纯判定。
///
/// 区间远小于计数器半程（2^31 tick ≈ 214 s @ 10 MHz），所以 wrapping 差值在
/// 同一半程内就是真实差值；越过半程 = 陈旧/倒退时间戳，丢弃。
fn classify(entry_low: u32, start_low: u32) -> Sample {
    let delta = entry_low.wrapping_sub(start_low);
    if delta > (1u32 << 31) {
        return Sample::Backwards;
    }
    Sample::Interval(u64::from(delta))
}

/// 触发一次并等待 handler 入口。
///
/// 协议：masked IER 下先写 THR（占位字节）→ 读 `start` → 写 IER=THRE（触发）
/// → 有界自旋等 `SERVED`。返回 `None` = 超时（此时主动 mask 源）。
fn sample_once(state: *mut State, timeout: u64) -> Option<Sample> {
    let ptr = unsafe { (*state).irq_uart_lease.load(Ordering::SeqCst) } as *mut u8;
    if ptr.is_null() {
        return Some(Sample::Missing);
    }
    unsafe {
        (*state).irq_served.store(false, Ordering::SeqCst);
        (*state).irq_entry_low.store(0, Ordering::SeqCst);
    }

    // masked IER 下先写 THR（占位字节）——这是准备，不是触发点；读 `start`
    // 后写 IER=THRE 才是触发。THR 写不计入区间（与模块头的协议一致）。
    // SAFETY: 同上；offset 0 = THR，offset 1 = IER（字节编址，见模块头）。
    unsafe { core::ptr::write_volatile(ptr.add(REG_RBR_THR), 0) };
    let start = unsafe { abi::kcore_now() };
    let start_low = start as u32;
    unsafe { core::ptr::write_volatile(ptr.add(REG_IER), IER_THRE) };

    while !unsafe { (*state).irq_served.load(Ordering::SeqCst) } {
        if unsafe { abi::kcore_now() }.wrapping_sub(start) > timeout {
            unsafe { core::ptr::write_volatile(ptr.add(REG_IER), 0) };
            // 超时时若 handler 恰好刚到，仍按真实样本处理（不丢好数据）。
            if unsafe { (*state).irq_served.load(Ordering::SeqCst) } {
                return Some(classify(
                    unsafe { (*state).irq_entry_low.load(Ordering::SeqCst) },
                    start_low,
                ));
            }
            return None;
        }
    }
    let entry_low = unsafe { (*state).irq_entry_low.load(Ordering::SeqCst) };
    // 下一次触发前保持 mask（handler 已写 0；再写一次是幂等的保险）。
    unsafe { core::ptr::write_volatile(ptr.add(REG_IER), 0) };
    Some(classify(entry_low, start_low))
}

/// 报告一个被阻塞/不可用的块（没有合法 authority 时不硬测）。
fn blocked(status: &str, error: i32) {
    report::bench_header(NAME);
    report::key_str("method", "one_shot");
    report::key_str("trigger", "ns16550a_tx_empty");
    if error != 0 {
        report::key_i64("error", i64::from(error));
    }
    report::key_str("status", status);
}

/// 报告 lease 派生成功、但设备窗口不覆盖本 primitive 要触碰的寄存器。
///
/// Core **没有**拒绝派生——不能把它伪装成 `lease_failed`：如实给出观察到的
/// `(ptr, len)` 与所需长度，status 单独命名（`mmio_window_too_small`）。
fn blocked_window(ptr: usize, len: usize) {
    report::bench_header(NAME);
    report::key_str("method", "one_shot");
    report::key_str("trigger", "ns16550a_tx_empty");
    report::key_u64("lease_ptr", ptr as u64);
    report::key_u64("lease_len", len as u64);
    report::key_u64("required_len", MIN_HOST_LEN as u64);
    report::key_str("status", "mmio_window_too_small");
}

/// 释放本 primitive 认领的设备（顺序：irq release → device release；device
/// release 在仍有 live IRQ route 时会拒绝），并清零 state 里的记录。正常采样
/// 结束与每条失败路径都走它；destroy 兜底再调一次（幂等）。
fn release_authority(state: *mut State) {
    let device = unsafe { (*state).irq_device };
    if device != 0 {
        let _ = unsafe { abi::kcore_irq_release(device) };
        let _ = unsafe { abi::kcore_device_release(device) };
        unsafe { (*state).irq_device = 0 };
    }
    unsafe { (*state).irq_uart_lease.store(0, Ordering::SeqCst) };
}

/// destroy 的兜底 quiesce：清 handler 的 lease / SERVED 标记，并释放可能残留的
/// authority。正常 create 已在 [`run`] 内同步释放（设备已 mask、IRQ 已 release），
/// 所以这是幂等空操作；防御未来改动漏放，保证 destroy 时没有 live authority。
pub(crate) fn quiesce(state: *mut State) {
    unsafe {
        (*state).irq_uart_lease.store(0, Ordering::SeqCst);
        (*state).irq_served.store(false, Ordering::SeqCst);
    }
    release_authority(state);
}

/// 执行一次 `irq.uart_trigger_to_handler`（从 `kcomp_instance_create` 的锚点上下文调用）。
///
/// `state` 是本实例的 per-run 状态；注册 handler 时作为 `ctx` 原样回传，handler
/// 只经 `ctx` 读状态（不再读 image-global static）。
pub(crate) fn run(state: *mut State, context: &Context) {
    // ---- 合法 authority 链（每一步都如实报告失败在哪） ----
    let mut device = 0u32;
    if unsafe {
        abi::kcore_device_nth(
            UART_COMPATIBLE.as_ptr(),
            UART_COMPATIBLE.len(),
            0,
            &mut device,
        )
    } != 0
    {
        blocked("no_device", 0);
        return;
    }
    let mut ptr = core::ptr::null_mut();
    let mut len = 0usize;
    let claimed = unsafe { abi::kcore_device_claim(device, &mut ptr, &mut len) };
    if claimed != 0 {
        // 设备已被其它组件持有（quarantine / 已 claim）；这**不是**需要 workaround
        // 的障碍——如实报告缺的是"一台空闲的 ns16550a"。
        blocked("device_not_owned", claimed);
        return;
    }
    // 记录认领到的设备身份：destroy 用它兜底 quiesce（正常路径下面同步释放清零）。
    unsafe { (*state).irq_device = device };
    if ptr.is_null() || len < MIN_HOST_LEN {
        // claim 成功但窗口不覆盖要碰的寄存器，如实报告窗口本身。
        release_authority(state);
        blocked_window(ptr as usize, len);
        return;
    }
    unsafe {
        (*state)
            .irq_uart_lease
            .store(ptr as usize, Ordering::SeqCst)
    };
    let registered = unsafe { abi::kcore_irq_register(device, handler, state as *mut ()) };
    if registered != 0 {
        release_authority(state);
        blocked("register_failed", registered);
        return;
    }
    // IER 是字节寄存器（偏移 1），开局先 mask。
    unsafe { core::ptr::write_volatile(ptr.add(REG_IER), 0) };
    let enabled = unsafe { abi::kcore_irq_enable(device) };
    if enabled != 0 {
        release_authority(state);
        blocked("enable_failed", enabled);
        return;
    }

    // ---- 采样（warmup 不计入；超时/倒退样本丢弃并计数） ----
    let hz = unsafe { abi::kcore_timebase_hz() };
    let timeout = measure::ticks_per_millisecond(hz)
        .saturating_mul(TIMEOUT_MS)
        .max(1_000);

    let mut warmups = 0u64;
    while warmups < WARMUP_SAMPLES {
        let _ = sample_once(state, timeout);
        warmups += 1;
    }

    let mut samples = [0u64; crate::SAMPLES];
    let mut valid = 0usize;
    let mut discarded_timeouts = 0u64;
    let mut discarded_backwards = 0u64;
    let mut truncated = false;
    while valid < crate::SAMPLES {
        match sample_once(state, timeout) {
            Some(Sample::Interval(interval)) => {
                samples[valid] = interval;
                valid += 1;
            }
            Some(Sample::Backwards) => discarded_backwards += 1,
            Some(Sample::Missing) | None => discarded_timeouts += 1,
        }
        let discarded = discarded_timeouts + discarded_backwards;
        if discarded > MAX_DISCARDS || (discarded_timeouts > MAX_TIMEOUTS && valid < crate::SAMPLES)
        {
            truncated = true;
            break;
        }
    }

    // 先关源、断线、还设备（顺序：irq release → mmio release；mmio release 在
    // 仍有 live IRQ child 时会拒绝）。关源走 lease 字节写（同上）。
    unsafe { core::ptr::write_volatile(ptr.add(REG_IER), 0) };
    unsafe { (*state).irq_uart_lease.store(0, Ordering::SeqCst) };
    release_authority(state);

    report::bench_header(NAME);
    report::key_str("method", "one_shot");
    report::key_str("unit", "timebase-ticks");
    report::key_str("sample_unit", "trigger_to_handler_entry");
    report::key_str("trigger", "ns16550a_tx_empty");
    report::key_u64("warmup_samples", warmups);
    report::key_u64("samples", valid as u64);
    report::key_u64("discarded_timeouts", discarded_timeouts);
    report::key_u64("discarded_backwards", discarded_backwards);
    if truncated || valid < crate::SAMPLES {
        report::key_str("status", "trigger_timeout");
        return;
    }
    report::key_u64("rounds", crate::ROUNDS as u64);
    report::key_u64("batches_per_round", crate::BATCHES_PER_ROUND as u64);
    report::key_u64("clock_quantum", context.quantum);
    report::key_u64("clock_probe_reads", context.probe.reads);
    report::key_u64("clock_observed_min_delta", context.probe.min_positive);
    report::key_u64("clock_median_read_delta", context.probe.median_positive);
    report::key_u64("clock_bracket_min", context.bracket);

    // 每个 round = 连续 31 个样本的独立中位数（看轮间散布）。
    let mut round_medians = [0u64; crate::ROUNDS];
    let mut round = 0usize;
    while round < crate::ROUNDS {
        let base = round * crate::BATCHES_PER_ROUND;
        let mut window = [0u64; crate::BATCHES_PER_ROUND];
        window.copy_from_slice(&samples[base..base + crate::BATCHES_PER_ROUND]);
        window.sort_unstable();
        round_medians[round] = window[crate::BATCHES_PER_ROUND / 2];
        round += 1;
    }

    // 低于"一次读钟成本"的样本 = 量化主导；用同一个诚实字段暴露。
    let floor = context.probe.median_positive.max(1);
    stats::sort(&mut samples);
    let stats = stats::summarize(&samples, floor);
    report::key_u64("floor_clock_read_delta", floor);
    report::key_u64("min", stats.min);
    report::key_u64("median", stats.median);
    report::key_u64("p95", stats.p95);
    report::key_u64("max", stats.max);
    report::key_u64("total", stats.total);
    let mut round = 0usize;
    while round < crate::ROUNDS {
        report::round_median(round as u64, round_medians[round]);
        round += 1;
    }
    report::key_u64("below_floor_samples", stats.below_floor);
    report::key_str(
        "resolution_limited",
        if stats.below_floor > 0 { "yes" } else { "no" },
    );
    // 没有等价的 null baseline：制造一个就等于抑制触发/关中断（禁止）。
    report::key_str("baseline", "none");
    report::key_str("status", "ok");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_accepts_positive_delta() {
        assert_eq!(classify(150, 100), Sample::Interval(50));
    }

    #[test]
    fn classify_accepts_zero_delta() {
        // 同一 tick 内完成：不是倒退，只是落在量化下限（由 below_floor 暴露）。
        assert_eq!(classify(100, 100), Sample::Interval(0));
    }

    #[test]
    fn classify_handles_low_32_bit_wrap() {
        // rdtime 低 32 位回绕：start=0xFFFF_FFF0、entry=0x10 → 真实区间 0x20。
        assert_eq!(classify(0x10, 0xFFFF_FFF0), Sample::Interval(0x20));
    }

    #[test]
    fn classify_discards_stale_backwards_timestamps() {
        // 越过半程 = 陈旧/倒退（entry 早于 start）：不做饱和减法，直接丢弃。
        assert_eq!(classify(90, 100), Sample::Backwards);
    }
}
