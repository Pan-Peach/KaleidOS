//! 最小系统 shell；Core / ComponentManager 拥有生命周期与资源真相。
#![no_std]

#[cfg(test)]
extern crate std;

mod input;
mod parser;
#[cfg(target_os = "none")]
mod runtime;
#[cfg(target_os = "none")]
mod shell;
