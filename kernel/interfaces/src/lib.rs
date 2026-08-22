//! Interface 契约 —— 组件提供的能力。
//!
//! Interface 是语义，传输（direct call / IPC / Wasm host call）是绑定策略。
//! 契约不绑定 native Rust ABI。见 `docs/component-model.md`。

#![no_std]

#[cfg(test)]
extern crate std;

pub mod device;
pub mod policy;
pub mod service;