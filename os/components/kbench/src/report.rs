//! 输出层：无除法十进制打印 + `key=value` 报告行。
//!
//! 组件里**不做除法**（避免 `__udivdi3` libcall，也满足"测量端只发原始整数，
//! 小数换算由 host 做"）：十进制用 10 的幂表逐位相减；符号只出现在 signed
//! 报告（配对差值）里。

use kcomp_sdk::console_write_byte;

/// 组件自己就知道的环境字段（runner 可再补充 platform / accelerator / commit）。
const ARCH: &str = if cfg!(target_arch = "riscv64") {
    "riscv64"
} else if cfg!(target_arch = "riscv32") {
    "riscv32"
} else {
    "unknown"
};

const POW10: [u64; 20] = [
    1,
    10,
    100,
    1_000,
    10_000,
    100_000,
    1_000_000,
    10_000_000,
    100_000_000,
    1_000_000_000,
    10_000_000_000,
    100_000_000_000,
    1_000_000_000_000,
    10_000_000_000_000,
    100_000_000_000_000,
    1_000_000_000_000_000,
    10_000_000_000_000_000,
    100_000_000_000_000_000,
    1_000_000_000_000_000_000,
    10_000_000_000_000_000_000,
];

pub(crate) fn write_str(text: &str) {
    for &byte in text.as_bytes() {
        console_write_byte(byte);
    }
}

pub(crate) fn write_u64(mut value: u64) {
    let mut started = false;
    let mut index = POW10.len();
    while index > 0 {
        index -= 1;
        let place = POW10[index];
        let mut digit = 0u8;
        while value >= place {
            value -= place;
            digit += 1;
        }
        if digit != 0 || started || index == 0 {
            console_write_byte(b'0' + digit);
            started = true;
        }
    }
}

pub(crate) fn write_i64(value: i64) {
    if value < 0 {
        console_write_byte(b'-');
        write_u64(value.unsigned_abs());
    } else {
        write_u64(value as u64);
    }
}

fn key(key: &str) {
    write_str(key);
    console_write_byte(b'=');
}

pub(crate) fn key_u64(key_text: &str, value: u64) {
    key(key_text);
    write_u64(value);
    console_write_byte(b'\n');
}

pub(crate) fn key_i64(key_text: &str, value: i64) {
    key(key_text);
    write_i64(value);
    console_write_byte(b'\n');
}

pub(crate) fn key_str(key_text: &str, value: &str) {
    key(key_text);
    write_str(value);
    console_write_byte(b'\n');
}

/// `round_<n>_median=<v>`：轮号参与 key（不预先假定只有一位轮号）。
pub(crate) fn round_median(round: u64, value: u64) {
    write_str("round_");
    write_u64(round);
    write_str("_median=");
    write_u64(value);
    console_write_byte(b'\n');
}

pub(crate) fn bench_header(name: &str) {
    write_str("BENCH ");
    write_str(name);
    console_write_byte(b'\n');
}

/// `BENCH-ENV`：没有这一行数字不可比。
///
/// 组件只知道 arch / XLEN / timebase / build mode + trace 的**运行时观测面**；
/// platform、加速器（TCG/KVM）、config hash / commit 由 runner 记录（组件不猜）。
/// 当前仓库时钟只有 `rdtime`，所以 `clock=rdtime` 是如实的。
///
/// # Trace 状态（数字里包不包含 trace 成本）
///
/// - `trace_mask`：Core 报告的运行时使能掩码（bit i ↔ kind i+1）。非 0 = 被测
///   路径可能真的 `emit`（记录 + 读钟 + ring 锁），数字包含这部分成本；
///   0 = 没有任何事件会被记录（只有"标记 + 分支"的过滤成本）。
/// - `trace_capacity`：ring 容量（records）。
/// - `trace_records`：本次启动到目前为止是否真的读到过记录。`yes` 证明 trace
///   被编译进来且至少有事件被记录；`no` 不能区分"编译期 CONFIG_TRACE=n"与
///   "运行时掩码全关"（ABI 上同形）——但两者对被测路径的影响都只是掩码过滤。
pub(crate) fn env(timebase_hz: u64, trace_mask: u64, trace_capacity: u64, trace_records: bool) {
    write_str("BENCH-ENV arch=");
    write_str(ARCH);
    write_str(" xlen=");
    write_u64(core::mem::size_of::<usize>() as u64 * 8);
    write_str(" platform=undetected clock=rdtime timebase_hz=");
    write_u64(timebase_hz);
    write_str(" mode=");
    write_str(if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    });
    write_str(" trace_mask=");
    write_u64(trace_mask);
    write_str(" trace_capacity=");
    write_u64(trace_capacity);
    write_str(" trace_records=");
    write_str(if trace_records { "yes" } else { "no" });
    console_write_byte(b'\n');
}
