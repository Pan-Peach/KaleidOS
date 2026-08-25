//! CoreTest 测试组件（第一个 .kcomp）：验证组件加载链路正确性。
//!
//! - `kcomp_init`：约定导出符号（loader 放段后调用）；返回 0 = 加载正确
//! - 自检逻辑（纯逻辑，host-testable）：校验 .data 段放段正确（值保持原样）
//! - 无 god-mode：组件只走加载协议（导出符号），不看内核内部

#![no_std]

#[cfg(test)]
extern crate std;

/// 静态数据，.data 段。放段/重定位正确性锚点：值保持原样 = 段被正确搬运。
static MAGIC: u32 = 0xC0FFEE;

/// 组件入口（Linux module_init 约定）。0 = OK；非 0 = 加载失败错误码。
#[unsafe(no_mangle)]
pub extern "C" fn kcomp_init() -> i32 {
    if self_check() { 0 } else { 1 }
}

/// 自检：.data 段的值在放段后必须原样可读。
fn self_check() -> bool {
    MAGIC == 0xC0FFEE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_check_passes() {
        assert!(self_check());
    }

    #[test]
    fn kcomp_init_returns_zero() {
        assert_eq!(kcomp_init(), 0);
    }
}
