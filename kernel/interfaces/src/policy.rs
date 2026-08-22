//! Policy Interface：SchedulerPolicy、FrameAllocatorPolicy、PageReplacementPolicy。
//! 策略只"提议"，最终由 Core 验证并 commit（见 `docs/core-philosophy.md`）。