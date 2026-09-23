//! prober-owned assignment cursor（纯逻辑，host-testable）。
//!
//! prober 把某驱动声明的 compatible 命中的 DeviceId 枚举进这里；
//! [`AssignmentCursor::next`] 是那条**游标**——**由 prober 自己的 dispatch 任务**
//! 逐台取出（每台对应一次 `kcore_component_create` 的 assignment）；
//! [`AssignmentCursor::report`] 记录该 attempt 的结果，同样是 prober 的**普通本地
//! 函数调用**（不是 driver 回调，也没有任何 endpoint 往返）。
//!
//! `attempt` 是 prober 分配的序号（1 起、唯一），既用于结果端口名
//! （`probe.result.<attempt>`），也用于拒绝 stale report——它**不是 capability**：
//! 既不是 handle，也不携带 authority。

/// 下发分配的上限（静态定长；无 alloc）。QEMU virt 有 8 台 `virtio,mmio`
/// transport，16 给“再来一类候选”留出余量。
pub const MAX_ASSIGNMENTS: usize = 16;

#[derive(Clone, Copy)]
struct Assignment {
    driver: &'static [u8],
    device_id: u32,
    attempt: u32,
    handed: bool,
    reported: bool,
    outcome: i32,
    detail: u32,
}

const EMPTY: &[u8] = &[];
const EMPTY_SLOT: Assignment = Assignment {
    driver: EMPTY,
    device_id: 0,
    attempt: 0,
    handed: false,
    reported: false,
    outcome: 0,
    detail: 0,
};

/// [`AssignmentCursor::report`] 的失败原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportError {
    /// `attempt` 从未由本 prober 分配（stale / 外来上报）。
    UnknownAttempt,
    /// `attempt` 已分配但还没下发（正常观察不到）。
    NotHanded,
    /// `attempt` 已上报过（stale / 重复）。
    AlreadyReported,
}

/// 扁平的、有序的分配表 + 每个槽位的下发 / 上报状态（游标 = `handed`）。
pub struct AssignmentCursor {
    entries: [Assignment; MAX_ASSIGNMENTS],
    count: usize,
}

impl AssignmentCursor {
    pub const fn new() -> Self {
        Self {
            entries: [EMPTY_SLOT; MAX_ASSIGNMENTS],
            count: 0,
        }
    }

    /// 追加一台候选设备；attempt = `下标 + 1`。表满返回 `false`（调用方记日志，
    /// 不覆盖已有项）。
    pub fn push(&mut self, driver: &'static [u8], device_id: u32) -> bool {
        if self.count >= MAX_ASSIGNMENTS {
            return false;
        }
        self.entries[self.count] = Assignment {
            driver,
            device_id,
            attempt: self.count as u32 + 1,
            handed: false,
            reported: false,
            outcome: 0,
            detail: 0,
        };
        self.count += 1;
        true
    }

    /// 按 push 顺序取出 `driver` 的下一台**尚未下发**的设备，返回
    /// `(attempt, device_id)`；没有更多返回 `None`（dispatch 任务据此结束该候选）。
    /// 取出后该项即为"已下发"——结果经 [`Self::report`] 本地记录。
    pub fn next(&mut self, driver: &[u8]) -> Option<(u32, u32)> {
        for entry in self.entries[..self.count].iter_mut() {
            if entry.driver == driver && !entry.handed {
                entry.handed = true;
                return Some((entry.attempt, entry.device_id));
            }
        }
        None
    }

    /// 记录某个已下发 `attempt` 的结果（`outcome` 编码见 SDK `probe` 模块）。
    pub fn report(&mut self, attempt: u32, outcome: i32, detail: u32) -> Result<(), ReportError> {
        let Some(entry) = self.entries[..self.count]
            .iter_mut()
            .find(|entry| entry.attempt == attempt)
        else {
            return Err(ReportError::UnknownAttempt);
        };
        if !entry.handed {
            return Err(ReportError::NotHanded);
        }
        if entry.reported {
            return Err(ReportError::AlreadyReported);
        }
        entry.reported = true;
        entry.outcome = outcome;
        entry.detail = detail;
        Ok(())
    }

    /// 是否每个已 push 的分配都已上报（诊断 / 测试用）。
    pub fn all_reported(&self) -> bool {
        self.entries[..self.count]
            .iter()
            .all(|entry| entry.reported)
    }
}

impl Default for AssignmentCursor {
    fn default() -> Self {
        Self::new()
    }
}
