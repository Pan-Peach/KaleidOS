#![no_std]
#![no_main]

#[cfg(target_arch = "riscv32")]
#[path = "main32.rs"]
mod rv32;

#[cfg(target_arch = "riscv64")]
#[path = "main64.rs"]
mod rv64;

// RV64 内核地址空间：bootstrap（临时）+ runtime（长期，骨架）+ layout（共同输入）。
#[cfg(target_arch = "riscv64")]
#[path = "vm/mod.rs"]
pub(crate) mod vm;

#[cfg(feature = "selftest")]
#[path = "selftest.rs"]
mod selftest;
