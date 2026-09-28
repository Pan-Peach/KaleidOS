//! CPU 启动状态机与 BSP/AP 启动屏障。
//!
//! 启动协议（实现阶段）：
//!
//! 1. BSP 把所有请求的 AP 置 `Starting` 后逐个请求启动；
//! 2. AP 绑定本地存储、初始化 cpu/controller/timer/ipi 后置 `Ready`，然后在
//!    屏障上**关中断**自旋等待；
//! 3. 所有 AP 达到 `Ready` 后 BSP 放行屏障；
//! 4. AP 转入 `Online` 后才打开正常中断投递并进入 Core 空闲/调度路径。
//!
//! fail-closed：放行前任一 AP 失败即整体失败，已启动的 AP 保持 parked；迟到的
//! AP **不得**把 `Failed` 覆写成 `Ready`。

use core::sync::atomic::{AtomicBool, Ordering};

/// 单个 CPU 的启动状态。
///
/// 原子编码（[`Self::as_raw`] / [`Self::from_raw`]）供 `AtomicU8` 使用。
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpuBootState {
    /// 尚未请求启动。
    Offline = 0,
    /// 已请求启动，AP 尚未 Ready。
    Starting = 1,
    /// AP 已完成本地初始化，正在等待启动屏障放行。
    Ready = 2,
    /// 已放行并完成启动，可接受正常中断/调度。
    Online = 3,
    /// 启动失败（终态；迟到入口不得复活）。
    Failed = 4,
}

impl CpuBootState {
    /// 编码为 `AtomicU8` 可存的值。
    pub const fn as_raw(self) -> u8 {
        self as u8
    }

    /// 由原子存储值解码；非法值返回 `None`。
    pub const fn from_raw(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(Self::Offline),
            1 => Some(Self::Starting),
            2 => Some(Self::Ready),
            3 => Some(Self::Online),
            4 => Some(Self::Failed),
            _ => None,
        }
    }
}

/// BSP ↔ AP 的启动屏障：AP 在 `wait` 上自旋，BSP 在全部 Ready 后 `release`。
///
/// 单原子自旋对本阶段足够（Oracle：a simple atomic spin gate is sufficient）；
/// 超时判定由 Core 的 `wait_until_online` 负责。
#[derive(Debug, Default)]
pub struct BootGate {
    released: AtomicBool,
}

impl BootGate {
    /// 构造一个未放行的屏障。
    pub const fn new() -> Self {
        Self {
            released: AtomicBool::new(false),
        }
    }

    /// 放行（BSP 调用一次）。
    pub fn release(&self) {
        self.released.store(true, Ordering::Release);
    }

    /// 是否已放行。
    pub fn is_released(&self) -> bool {
        self.released.load(Ordering::Acquire)
    }

    /// AP 侧自旋等待放行。调用者负责在等待前后保持中断关闭。
    pub fn wait(&self) {
        while !self.is_released() {
            core::hint::spin_loop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_state_round_trips_through_raw() {
        for state in [
            CpuBootState::Offline,
            CpuBootState::Starting,
            CpuBootState::Ready,
            CpuBootState::Online,
            CpuBootState::Failed,
        ] {
            assert_eq!(CpuBootState::from_raw(state.as_raw()), Some(state));
        }
        assert_eq!(CpuBootState::from_raw(5), None);
    }

    #[test]
    fn gate_is_closed_until_released() {
        let gate = BootGate::new();
        assert!(!gate.is_released());
        gate.release();
        assert!(gate.is_released());
    }
}
