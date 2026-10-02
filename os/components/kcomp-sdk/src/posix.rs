//! POSIX personality 的组件配置声明；不提供 libc，也不定义 Linux syscall ABI。
//! 配置由组合者提供，endpoint 的存活 / contract / ABI 仍经 Core 校验。
//! 配置 wire 为 LE 字节，不能直接 cast config 指针到 Rust struct。
pub use crate::generated::posix::*;
