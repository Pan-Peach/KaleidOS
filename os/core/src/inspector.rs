//! TestInspector —— Core 对外暴露的**只读**测试观察口（见 docs/testing.md）。
//!
//! 约束（Oracle 审查结论）：只能读取 Core 状态用于断言，不能修改任何状态（无 god-mode）；
//! 返回**快照副本**而非内部引用；构造器仅 Core 私有，测试组合收到已建好的只读门面。
//!
//! 计划接口（M1 随第一个真相存储落地）：
//! - task(TaskId) -> Option<TaskSnapshot>         状态/owner/generation/CPU
//! - frame(FrameId) -> Option<FrameSnapshot>      状态/owner/generation
//! - component(ComponentId) -> Option<ComponentSnapshot>
//! - visit_trace_since(seq, visitor)              只读遍历 trace
