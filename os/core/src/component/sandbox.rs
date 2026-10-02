//! SandboxedNative 执行域的 Core 接缝，不是可加载组件或 POSIX service。
//! 目标是低特权 + 私有 AS + task 绑定 + 用户 trap / ecall；机制尚未实现。
//! 契约见 docs/architecture/deployment.md 与 docs/architecture/driver-model.md §6.4；实现顺序见
//! docs/development/userspace.md。这里的 Rust 类型只供 Core 内部使用。

use crate::memory::address_space::{AddressSpaceHandle, VirtualRange};
use crate::task::TaskId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxError {
    Unsupported,
}

/// 装载者提议的用户入口；不能因为拿到这些数值就直接 sret。
pub struct UserEntry {
    pub pc: usize,
    pub stack: VirtualRange,
}

/// TODO: 装入 Core 已验证的 task / AS 关联与 Arch 激活现场。
/// 构造字段私有，当前任何入口都不能返回成功的 PreparedUser。
pub struct PreparedUser {
    _private: (),
}

/// TODO: 验证真实 task owner、AS 生命周期、U 位 / RX 入口 / RW-NX 栈与范围溢出；
/// 准备完成后无锁进入，调度提交与 AS 切换必须在 Core 中一致。
pub fn prepare_task(
    _task: TaskId,
    _space: AddressSpaceHandle,
    _entry: &UserEntry,
) -> Result<PreparedUser, SandboxError> {
    Err(SandboxError::Unsupported)
}

/// TODO: Arch 的 U-mode 进入 / trap 返回；不复用 S-mode trampoline 冒充隔离。
pub fn enter(_prepared: PreparedUser) -> Result<(), SandboxError> {
    Err(SandboxError::Unsupported)
}

/// TODO: 按实际调用 task 的 AS 验证逐页范围、权限、fault 与并发 unmap。
/// 用户地址不变成裸 Core 引用，不依赖 SUM；复制结果不得超过 destination.len()。
pub fn copy_from_user(
    _task: TaskId,
    _source: VirtualRange,
    _destination: &mut [u8],
) -> Result<usize, SandboxError> {
    Err(SandboxError::Unsupported)
}

pub fn copy_to_user(
    _task: TaskId,
    _destination: VirtualRange,
    _source: &[u8],
) -> Result<usize, SandboxError> {
    Err(SandboxError::Unsupported)
}

// TODO: 用户 trap 的真实来源 / return frame / fault 归因 / 逻辑退役，及
// Core mechanism ecall 与 personality syscall 的路由契约。Core 不解释 Linux 号、
// pid/fd、ELF、POSIX errno；跨组件回调必须另定窄 C ABI，不传 Rust trait / enum。
