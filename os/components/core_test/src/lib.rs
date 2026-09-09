//! CoreTest 测试组件（第一个 .kcomp）：核内自检 Core 的真实接口。
//!
//! - `kcomp_init`：loader 放段 + 重定位后调用；返回 0 = 全部通过，
//!   非 0 = 失败位图（`load` 命令会据此报告 FAILED）
//! - 自检项（核内，QEMU `load core_test` 时真实执行）：
//!   `.data` 段搬运 / 机器真相 / 内存分配器 / 组件注册表
//! - 只走导出白名单（`kcore_*`），无 god-mode
//! - host 测试只覆盖纯逻辑；真实执行在核内

#![no_std]

#[cfg(test)]
extern crate std;

/// 静态数据，.data 段。放段/重定位正确性锚点：值原样保持 = 段被正确搬运。
static MAGIC: u32 = 0xC0FFEE;

/// 纯逻辑（host-testable）：.data 段的值在放段后必须原样可读。
fn data_ok() -> bool {
    MAGIC == 0xC0FFEE
}

/// Validate the machine facts supplied by the Core exports.  The actual hart
/// membership lookup stays in Core; this pure predicate remains host-testable.
fn machine_ok(cpu_count: usize, boot_hart_present: bool) -> bool {
    cpu_count >= 1 && boot_hart_present
}

/// 核内自检：仅在非 test 编译生成（组件 .kcomp / 真机）。
/// host `cargo test` 不引用 `kcore_*`，避免未定义符号链接失败；
/// 组件镜像里的 UNDEF 符号由 loader 按导出白名单重定位解析。
#[cfg(not(test))]
mod runtime {
    use super::{data_ok, machine_ok};

    // 白名单 API（与 kernel `export.rs` 一一对应；C ABI 声明即契约）。
    unsafe extern "C" {
        #[link_name = "kcore_console_write_byte"]
        fn console_write_byte(byte: u8);
        #[link_name = "kcore_machine_boot_hart"]
        fn machine_boot_hart() -> usize;
        #[link_name = "kcore_machine_cpu_count"]
        fn machine_cpu_count() -> usize;
        #[link_name = "kcore_machine_has_hart"]
        fn machine_has_hart(hart_id: usize) -> i32;
        #[link_name = "kcore_free_page_count"]
        fn free_page_count() -> usize;
        #[link_name = "kcore_component_count"]
        fn component_count() -> usize;
    }

    fn puts(s: &str) {
        for &b in s.as_bytes() {
            unsafe {
                console_write_byte(b);
            }
        }
    }

    /// 报告一项检查：`[core-test] <name>: PASS|FAIL\n`。
    fn report(name: &str, ok: bool) {
        puts("[core-test] ");
        puts(name);
        puts(": ");
        puts(if ok { "PASS\n" } else { "FAIL\n" });
    }

    /// 内存分配器已初始化（存在可分配空闲页）。
    fn memory_ok() -> bool {
        let free = unsafe { free_page_count() };
        free > 0
    }

    /// 组件注册表已登记（至少包含当前组件）。
    fn component_ok() -> bool {
        let count = unsafe { component_count() };
        count >= 1
    }

    /// 组件入口（Linux module_init 约定）：0 = 全部通过；非 0 = 失败位图。
    #[unsafe(no_mangle)]
    pub extern "C" fn kcomp_init() -> i32 {
        let mut failed = 0u32;

        let ok = data_ok();
        failed |= !ok as u32;
        report("data", ok);

        let cpus = unsafe { machine_cpu_count() };
        let boot = unsafe { machine_boot_hart() };
        let boot_present = unsafe { machine_has_hart(boot) } != 0;
        let ok = machine_ok(cpus, boot_present);
        failed |= (!ok as u32) << 1;
        report("machine", ok);

        let ok = memory_ok();
        failed |= (!ok as u32) << 2;
        report("memory", ok);

        let ok = component_ok();
        failed |= (!ok as u32) << 3;
        report("component", ok);

        report("all", failed == 0);
        failed as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_ok_passes() {
        assert!(data_ok());
    }

    #[test]
    fn machine_check_accepts_sparse_hart_ids() {
        assert!(machine_ok(2, true));
    }

    #[test]
    fn machine_check_rejects_missing_boot_hart() {
        assert!(!machine_ok(2, false));
    }
}
