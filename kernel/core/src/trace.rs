//! 结构化 Trace —— 断言与未来确定性重放的证据基础（见 docs/testing.md）。
//!
//! 记录形态（Oracle 审查结论）：`TraceRecord { sequence, event }`，
//! sequence 为 Core 分配的单调序号（断言排序依据；墙钟时间只作元数据）；
//! event 为类型化事件（task switch / block / wake / grant / revoke / 组件生命周期 /
//! IRQ / fault / policy proposal / Core rejection），含类型化 ID 与理由，
//! 不用格式化字符串或指针。
//!
//! 目标端用固定容量环形缓冲；溢出必须显式标记（overflow marker），
//! 不允许静默丢事件导致断言误导。