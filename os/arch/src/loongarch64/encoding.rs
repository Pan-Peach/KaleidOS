//! loongarch64 纯编码（宿主可编译、可单测；硬件访问不在此处）。
//!
//! 只放**格式**：CSR 编号、IOCSR IPI/mailbox 偏移、TCFG 位、mailbox 打包。
//! 计算函数体为 `todo!()`。
//!
//! # 已验证性
//!
//! 骨架：CSR 编号按 LoongArch 架构手册；IOCSR 偏移按 Linux
//! `arch/loongarch/include/asm/loongarch.h` 的常见定义。实现前请对照
//! **Linux + DragonOS** 复核（用户明确要求 loongarch 参考 DragonOS）。
//!
//! 注意：IOCSR 不是普通 MMIO（不经 discovery 的 `iocsr_base` 指针访问）；
//! `MBUF_SEND` 是**选中的某个 64 位 mailbox** 的发送口，高/低半字写到同一口；
//! `CNTC` 是计数器补偿，不是读取稳定计数器的方式（读用它 `RDTIME*`）。

/// `CSR.CRMD`（当前模式）。
pub const CSR_CRMD: u32 = 0x00;
/// `CSR.PRMD`（异常前模式）。
pub const CSR_PRMD: u32 = 0x01;
/// `CSR.ECFG`（异常配置 / 中断使能）。
pub const CSR_ECFG: u32 = 0x04;
/// `CSR.ESTAT`（异常状态）。
pub const CSR_ESTAT: u32 = 0x05;
/// `CSR.ERA`（异常返回地址）。
pub const CSR_ERA: u32 = 0x06;
/// `CSR.EENTRY`（异常入口）。
pub const CSR_EENTRY: u32 = 0x0C;
/// `CSR.CPUID`（当前核号）。
pub const CSR_CPUID: u32 = 0x20;
/// `CSR.KSAVE0`（内核暂存；per-CPU 基址候选槽）。
pub const CSR_KSAVE0: u32 = 0x30;
/// `CSR.TCFG`（timer 配置）。
pub const CSR_TCFG: u32 = 0x41;
/// `CSR.TVAL`（timer 当前值）。
pub const CSR_TVAL: u32 = 0x42;
/// `CSR.TINTCLR`（timer 中断清除）。
pub const CSR_TINTCLR: u32 = 0x44;

/// IOCSR IPI 状态寄存器偏移。
pub const IOCSR_IPI_STATUS: usize = 0x1000;
/// IOCSR IPI 使能寄存器偏移。
pub const IOCSR_IPI_EN: usize = 0x1004;
/// IOCSR IPI 置位寄存器偏移。
pub const IOCSR_IPI_SET: usize = 0x1008;
/// IOCSR IPI 清除寄存器偏移。
pub const IOCSR_IPI_CLEAR: usize = 0x100C;
/// IOCSR mailbox 0。
pub const IOCSR_MBUF0: usize = 0x1020;
/// IOCSR mailbox 1。
pub const IOCSR_MBUF1: usize = 0x1028;
/// IOCSR mailbox 2。
pub const IOCSR_MBUF2: usize = 0x1030;
/// IOCSR mailbox 3。
pub const IOCSR_MBUF3: usize = 0x1038;
/// IOCSR **IPI 发送**寄存器偏移（门铃；与 mailbox 不同）。
pub const IOCSR_IPI_SEND: usize = 0x1040;
/// IOCSR **mailbox 发送**寄存器偏移（选中 mailbox 的高/低半字写到这一口）。
pub const IOCSR_MBUF_SEND: usize = 0x1048;

/// `TCFG` 位：timer 使能。
pub const TCFG_EN: u64 = 1 << 0;
/// `TCFG` 位：周期模式。
pub const TCFG_PERIODIC: u64 = 1 << 1;

/// 由 `CPUID` 取核号（低 9 位是 core id）。
pub fn cpu_number(_cpuid: u64) -> u16 {
    todo!("loongarch64: extract the core number from CSR.CPUID")
}

/// 组装 `TCFG` 的 timer 配置字（init value + 周期位）。
pub fn encode_tcfg(_init_value: u64, _periodic: bool) -> u64 {
    todo!("loongarch64: encode a TCFG value")
}

/// 把一个 64 位值拆成 mailbox 发送的 (低半字, 高半字)。
///
/// 两半写到 `IOCSR_MBUF_SEND` 的同一选中 mailbox。注意 DragonOS 的用法是
/// entry 放 MBUF0、stack 放 MBUF1（两个独立值）——按选定的固件协议决定。
pub fn pack_mailbox_entry(_entry: u64) -> (u32, u32) {
    todo!("loongarch64: pack an AP entry address into mailbox words")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 骨架：实现后应通过；现在 `#[ignore]` 保持基线绿，`--ignored` 可见失败。
    #[test]
    #[ignore = "new ISA skeleton: implement pack_mailbox_entry"]
    fn mailbox_entry_round_trips_low_and_high_words() {
        let entry = 0x0000_0000_9000_1234u64;
        let (low, high) = pack_mailbox_entry(entry);
        assert_eq!(low, 0x9000_1234);
        assert_eq!(high, 0x0000_0000);
    }

    #[test]
    #[ignore = "new ISA skeleton: implement cpu_number"]
    fn cpuid_yields_core_number() {
        assert_eq!(cpu_number(0x3), 3);
    }
}
