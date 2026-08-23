use core::arch::asm;

const SBI_EXT_DBCN: usize = 0x4442_434E; // Debug Console Extension ("DBCN")
const SBI_DBCN_WRITE_BYTE: usize = 2;

/// Writes a byte to the debug console using the SBI Debug Console Extension.
pub fn dbcn_write_byte(byte: u8) {
    let mut a0 = byte as usize; // The byte to write
    let mut a1: usize; // Placeholder for the return value
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") a0,
            lateout("a1") a1,
            in("a6") SBI_DBCN_WRITE_BYTE,
            in("a7") SBI_EXT_DBCN,
        );
    }

    // SBI 返回：
    // a0: 错误码（0 表示成功，负数表示失败）
    // a1: 返回值（如果有的话）

    // bring-up 代码中不处理返回值，假设写入总是成功的。
    let _ = a0; // 忽略错误码
    let _ = a1; // 忽略返回值
}
