#![no_std]
#![no_main]

extern crate alloc;

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

// RV32 长期地址空间：bootstrap 的 4 GiB identity root 之后的真实 runtime root。
#[cfg(all(target_arch = "riscv32", feature = "vm-mmu"))]
#[path = "vm32/mod.rs"]
pub(crate) mod vm32;

// boot 本地链接地址归一化（RV32 boot + 两个 XLEN 的 selftest）。
#[cfg(any(target_arch = "riscv32", feature = "selftest"))]
#[path = "addr.rs"]
pub(crate) mod addr;

// 无堆早期内存 pass（RV64/RV32 共用）：包含镜像的 bank + FDT 排除区间扫描。
#[path = "bootmem.rs"]
pub(crate) mod bootmem;

// FDT 设备发现（RV64/RV32 共用）：完整中断资源 + PLIC 逻辑线绑定。
#[path = "discovery.rs"]
pub(crate) mod discovery;

// SMP 的 boot 侧骨架（AP trampoline + 栈 + 描述符 + 调用点）。仅 RV64 MMU。
// 恒编译：单 CPU 机器上 `start_secondaries` 自然空转；哪些用例跑由 runner 决定。
#[cfg(all(target_arch = "riscv64", feature = "vm-mmu"))]
#[path = "smp.rs"]
pub(crate) mod smp;

#[cfg(feature = "selftest")]
#[path = "selftest.rs"]
mod selftest;
