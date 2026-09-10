//! 内核虚拟地址空间：`layout` 是唯一段来源，`bootstrap`（临时静态页表）与
//! `runtime`（长期 buddy 动态页表）是两个阶段，共用同一 `KernelLayout`。
//!
//! ```text
//! main64.rs
//!    │
//!    ├── vm::layout      KernelLayout（linker symbols 唯一解释者）
//!    │       ↓
//!    ├── vm::bootstrap   临时 root（allocator 未起，静态池）
//!    │       ↓
//!    └── vm::runtime     长期 root（buddy 可用后）——骨架
//!            ↓
//!        arch::riscv::mmu   （Sv39 机制本身）
//! ```
//!
//! 整个模块是 RV64-only（`main.rs` 按 target_arch 门控；RV32 走 Sv32 identity，
//! 不经此处）。

pub mod bootstrap;
pub mod layout;
pub mod runtime;
