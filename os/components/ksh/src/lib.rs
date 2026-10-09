//! 有界交互 shell；Core / ComponentManager 拥有生命周期与资源真相。
#![no_std]
extern crate alloc;

#[cfg(test)]
extern crate std;

mod input;
mod parser;
#[cfg(target_os = "none")]
mod runtime;
#[cfg(target_os = "none")]
mod shell;
