#![no_std]
#![no_main]

#[cfg(target_arch = "riscv32")]
#[path = "main32.rs"]
mod rv32;

#[cfg(target_arch = "riscv64")]
#[path = "main64.rs"]
mod rv64;

#[cfg(feature = "selftest")]
#[path = "selftest.rs"]
mod selftest;
