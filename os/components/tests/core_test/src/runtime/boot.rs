//! 第 1 组（boot basics）：放段 / 机器真相 / 内存分配器 / 组件注册表。
//!
//! 全部是 Core 已提交真相的**只读**查询，不产生任何 mutation。

use kcomp_sdk::abi::{
    kcore_component_count, kcore_free_page_count, kcore_machine_boot_hart, kcore_machine_cpu_count,
    kcore_machine_has_hart,
};

use super::report::Checks;

pub fn group(checks: &mut Checks) {
    checks.group("boot basics");
    checks.check("data", crate::data_ok());
    let cpus = unsafe { kcore_machine_cpu_count() };
    let boot = unsafe { kcore_machine_boot_hart() };
    let boot_present = unsafe { kcore_machine_has_hart(boot) } != 0;
    checks.check("machine", crate::machine_ok(cpus, boot_present));
    checks.check("memory", unsafe { kcore_free_page_count() } > 0);
    checks.check("component", unsafe { kcore_component_count() } >= 1);
}
