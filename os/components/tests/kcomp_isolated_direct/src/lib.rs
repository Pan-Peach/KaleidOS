//! kcomp_isolated_direct —— Isolated **直接 import** 的 ArchTest **真实
//! `.kcomp`**。
//!
//! 与 `kcomp_isolated_life`（零 import，只用 Core 预置窗口）不同，本夹具在
//! 私有 AS 里**直接调用 Core 导出**：`kcore_now` / `kcore_machine_cpu_count` /
//! `kcore_component_count` / `kcore_free_page_count`（只读查询）与
//! `kcore_log_line`（诊断，经 `klog!`）。这些 import 命中
//! `isolated_load::SUPPORTED_IMPORTS`：装载时重定位到 Core 导出的**低别名**
//! （共享 identity RAM 在每个 Isolated AS 里映射它），运行时是普通 C-ABI 调用——
//! `satp` 保持实例 root，零切换。
//!
//! `config_abi == PANIC_ABI` 时 create 立刻 `panic!`：进入 SDK 的 panic adapter
//! （`kcore_log_line` + `kcore_panic_escape`），协作式逃逸回 Core 的跨 AS 延续，
//! 实例被收敛成 `Failed`。
//!
//! # 窗口协议（与 Core 一致）
//!
//! ```text
//! args     → 实例窗口基址 + 0（KcompCreateArgs）
//! out_state→ 实例窗口基址 + 32（组件写上报区地址，Core 从自己的视图读回）
//! report   → 实例窗口基址 + 512（magic / now / cpus / components / free / satp / tp / log）
//! ```

#![no_std]

use kcomp_sdk as _;

/// 上报区在实例窗口里的偏移（Core 不解释；ArchTest 按同一偏移读回）。
const REPORT_OFF: usize = 512;

const R_MAGIC: usize = 0;
const R_NOW: usize = 1;
const R_CPUS: usize = 2;
const R_COMPONENTS: usize = 3;
const R_FREE: usize = 4;
const R_SATP: usize = 5;
const R_TP: usize = 6;
const R_LOG: usize = 7;

const DIRECT_MAGIC: usize = 0x4449_5245; // "DIRE"
/// 故障注入：create 见到这个 config_abi 就 `panic!`（SDK panic adapter →
/// `kcore_panic_escape` → 跨 AS 延续）。
const PANIC_ABI: u64 = 0xDEAD_F00D;

fn read_tp() -> usize {
    let tp: usize;
    // SAFETY: 只读寄存器，无内存 / 栈副作用。
    unsafe {
        core::arch::asm!("mv {tp}, tp", tp = out(reg) tp, options(nomem, nostack, preserves_flags));
    }
    tp
}

fn read_satp() -> usize {
    let satp: usize;
    // SAFETY: 只读 CSR。
    unsafe {
        core::arch::asm!(
            "csrr {satp}, satp",
            satp = out(reg) satp,
            options(nostack, preserves_flags),
        );
    }
    satp
}

kcomp_sdk::kcomp_instance_create!(|args, out_state| {
    // SAFETY: Core 交付窗口内（args = 窗口基址 + 0）。config_abi 是普通 u64。
    let config_abi = unsafe { (*args).config_abi };
    if config_abi == PANIC_ABI {
        panic!("kcomp_isolated_direct: deliberate panic");
    }

    // 直接调用 Core 导出：命中支持面，重定位到共享的低别名。
    let now = unsafe { kcomp_sdk::abi::kcore_now() };
    let cpus = unsafe { kcomp_sdk::abi::kcore_machine_cpu_count() };
    let components = unsafe { kcomp_sdk::abi::kcore_component_count() };
    let free = unsafe { kcomp_sdk::abi::kcore_free_page_count() };

    // SAFETY: 上报区在实例窗口内（Core 预置、本实例私有映射）。
    let slots = (args as usize + REPORT_OFF) as *mut usize;
    unsafe {
        slots.add(R_MAGIC).write_volatile(DIRECT_MAGIC);
        slots.add(R_NOW).write_volatile(now as usize);
        slots.add(R_CPUS).write_volatile(cpus as usize);
        slots.add(R_COMPONENTS).write_volatile(components as usize);
        slots.add(R_FREE).write_volatile(free as usize);
        slots.add(R_SATP).write_volatile(read_satp());
        slots.add(R_TP).write_volatile(read_tp());
        slots.add(R_LOG).write_volatile(0);
        // SAFETY: out_state 是 Core 交付的窗口内槽（+32）；写上报区地址。
        out_state.write((args as usize + REPORT_OFF) as *mut ());
    }
    // 诊断走 `kcore_log_line`（支持面内）；ArchTest 断言这行出现在串口。
    kcomp_sdk::klog!("direct-ok");
    0
});

kcomp_sdk::kcomp_instance_destroy!(|state| {
    if !state.is_null() {
        // SAFETY: state 是 create 写回的上报区地址（同一实例，仍然映射）。
        let slots = state as *mut usize;
        unsafe {
            slots.add(R_LOG).write_volatile(1);
        }
    }
    0
});
