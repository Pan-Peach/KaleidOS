//! M-mode trap 入口骨架（`mtvec`/`mcause`/`mepc`/`mtval`）。
//!
//! 用于未来 RV32 M-mode / NoMMU bare-metal profile（无 OpenSBI 委托）。
//! 与 `supervisor` 共享本目录 `mod.rs` 的 `Trap`/`TrapFrame`/`Scause` 解码
//! （cause 编码在 S/M 模式一致）。
//!
//! TODO(实现，骨架已就位)：
//! - `mtvec` 安装（Direct 模式）+ `mcause`/`mepc`/`mtval` 读取路径；
//! - M-mode `TrapFrame` 布局（`mstatus` 替代 `sstatus` 的对应字段）；
//! - M-mode `trap_handler`（可复用 `super::Scause::cause` 解码）；
//! - 需要时配套 `trap32.S`/`trap64.S` 的 M-mode 变体（汇编 entry 换
//!   `csrw mtvec`，保存 `mcause`/`mepc`/`mtval`）。
//!
//! 编译期由 boot profile 选择（同 `entry32.S/entry64.S` 思路），不引入动态抽象。
