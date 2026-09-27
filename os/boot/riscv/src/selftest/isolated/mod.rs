//! Isolated 域 ArchTest（私有 AS 切换 / trap 往返 / 组件故障分派 / 按域装载 +
//! 页级权限强制）。这里直接驱动 Core 准备 + arch 准备/切换原语 +
//! `component::isolated_load`，证明机制本身（不经组件创建路径）。
//!
//! 用例按职责拆到子模块：
//!
//! ```text
//! fixtures   私有 AS / 控制页 / 实例栈 / 寄存器探针（各组的公共夹具）
//! mappings   单页映射与"实例 AS 里不可达"断言
//! transition Core → 私有 AS → Core 同步往返
//! traps      私有 AS 内的 trap 往返（timer / 可恢复缺页 / 拒绝恢复）
//! image      真实 `.kcomp` 的按域装载 + 段权限强制
//! lifecycle  生产 create → Ready → destroy 与 create/destroy 失败终态
//! service    KernelNative → Isolated 跨域 service（caller 帧直接交付 + 故障收敛）
//! failure    放段 / config / prepare 拒绝、destroy 故障、stale 访问阻断
//! restart    已服务实例的故障收敛 + 同镜像逻辑重启
//! ```
//!
//! `isolated32.S` / `isolated64.S` 是与 XLEN 对称的测试夹具入口。

use super::{fail, pass};
use arch::Timer;
use core::arch::global_asm;
use core::sync::atomic::{AtomicUsize, Ordering};
use kernel::component::isolated::{
    self, ComponentFault, FaultDecision, IsolatedPrepareError, Outcome, PreparedTransition,
};
use kernel::component::isolated_load::{self, PlacedImage, PlacedSegment};
use kernel::component::ComponentId;
use kernel::memory::address_space::{
    self, AddressSpaceHandle, Mapping, MappingPermission, PhysicalRange, VirtualRange,
};

#[cfg(target_arch = "riscv64")]
global_asm!(include_str!("isolated64.S"));
#[cfg(target_arch = "riscv32")]
global_asm!(include_str!("isolated32.S"));

unsafe extern "C" {
    static isolated_fixture_start: u8;
    static isolated_fixture_end: u8;
    fn isolated_roundtrip_entry();
    fn isolated_core_direct_entry();
    fn isolated_nested_a_entry();
    fn isolated_nested_b_entry();
    fn isolated_nested_b_fault_entry();
    fn isolated_timer_entry();
    fn isolated_fault_entry();
    fn isolated_abandon_entry();
    fn isolated_roundtrip_probe();
}

mod failure;
mod fixtures;
mod image;
mod imports;
mod lifecycle;
mod mappings;
mod nested;
mod restart;
mod service;
mod transition;
mod traps;

pub(crate) use failure::*;
pub(crate) use fixtures::*;
pub(crate) use image::*;
pub(crate) use imports::*;
pub(crate) use lifecycle::*;
pub(crate) use mappings::*;
pub(crate) use nested::*;
pub(crate) use restart::*;
pub(crate) use service::*;
pub(crate) use transition::*;
pub(crate) use traps::*;
