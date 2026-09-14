#![no_std]
#![no_main]

#[cfg(all(target_arch = "riscv64", feature = "vm-nommu"))]
compile_error!("the current RV64 boot layout requires `vm-mmu`; NoMMU boot is RV32-only");

#[cfg(all(feature = "selftest", feature = "vm-nommu"))]
compile_error!("the current architectural selftests require `vm-mmu`");

#[cfg(target_arch = "riscv32")]
#[path = "main32.rs"]
mod rv32;

#[cfg(all(target_arch = "riscv64", feature = "vm-mmu"))]
#[path = "main64.rs"]
mod rv64;

// RV64 内核地址空间：bootstrap（临时）+ runtime（长期，骨架）+ layout（共同输入）。
#[cfg(all(target_arch = "riscv64", feature = "vm-mmu"))]
#[path = "vm/mod.rs"]
pub(crate) mod vm;

#[cfg(feature = "selftest")]
#[path = "selftest.rs"]
mod selftest;
