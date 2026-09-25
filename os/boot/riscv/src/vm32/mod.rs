//! RV32 长期内核地址空间（Sv32，identity）：bootstrap 的 4 GiB 全量 executable
//! identity root 只活在 `kernel::init()` 之前；此后本模块用 buddy 动态页表建立
//! 长期 root（RAM / 镜像段权限 / 设备 MMIO），并把它交给 Core 的共享映射真相
//! （`kernel::memory::kernel_mappings::install`）。
//!
//! 为什么必须换 root：Isolated AS 的共享 Core 映射**不能**从 bootstrap 的
//! "全部 4 GiB executable identity" 推导——那会把别的实例的私有 backing 经
//! identity 别名暴露出去（`docs/architecture/deployment.md` §6.3）。
//!
//! ```text
//!   linker32.ld → vm32::layout::kernel_layout32() → KernelLayout32
//!                                                       │
//!                                          vm32::runtime::init（buddy 可用后）
//!                                                       ↓
//!                                    arch::AddressSpaceImpl（Sv32）+ 共享映射计划
//! ```

pub mod layout;
pub mod runtime;
