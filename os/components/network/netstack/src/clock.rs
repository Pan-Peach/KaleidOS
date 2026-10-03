//! Core tick ↔ smoltcp Instant 的组件内 adapter；不引入 wall clock 或隐式超时。

use smoltcp::time::Instant;

use crate::Result;

pub struct Clock {
    pub timebase_hz: u64,
}

impl Clock {
    pub fn acquire() -> Result<Self> {
        todo!("读取 kcore_timebase_hz；0 返回 ClockUnavailable，不猜测频率")
    }

    pub fn now(&self) -> Result<Instant> {
        todo!("读取 kcore_now，以宽整数换算为 smoltcp 微秒，检查 i64 溢出")
    }

    pub fn deadline_ticks(&self, _deadline: Instant) -> Result<u64> {
        todo!("绝对 Instant 换算为 Core tick，向上取整并校验负值 / 溢出")
    }
}
