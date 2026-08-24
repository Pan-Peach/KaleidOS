//! RISC-V SBI 调用的最小封装（Supervisor 侧，M-mode OpenSBI）。
//!
//! 使用 `sbi-rt`（rustsbi 组织，SBI 2.0 规范库）：提供 `console_write_byte`。
//! 将来需要 HSM（多核 hart 启动）/ IPI / Timer 扩展时直接用 `sbi_rt::hsm::*` 等
//! 现成接口，不必手写 ecall。

#[cfg(target_arch = "riscv64")]
pub fn debug_console_write_byte(byte: u8) {
    let _ = sbi_rt::console_write_byte(byte);
}

#[cfg(not(target_arch = "riscv64"))]
pub fn debug_console_write_byte(_byte: u8) {}
