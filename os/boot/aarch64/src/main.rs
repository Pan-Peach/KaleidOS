//! aarch64 boot: `-kernel` ELF entry → FDT discovery → `MachineInfo` →
//! `kernel::init` → ArchTest harness.
//!
//! QEMU contract (aarch64 `virt`, `-kernel <elf>`): the ELF is loaded at its
//! link addresses (0x40000000), the PC starts at `_start`, the MMU is off, and
//! no stack is provided (the entry assembly sets one up, see `entry.S`).
//! QEMU 5.2 does **not** pass the DTB in x0 for ELF kernels — discovery finds
//! and stages it (see `discovery.rs`).  This crate stays identity-mapped end to
//! end; page tables are explicitly out of scope (`arch::aarch64::mmu` is
//! `todo!()`).
//!
//! Single-image model (same as RISC-V): bootstrap stage + Core are linked into
//! one `kaleidos-aarch64-core`; the hand-off is the `kernel::init(&MachineInfo)`
//! function call, not a loader.

#![no_std]
#![no_main]

use arch::{Console, CpuArch};
use core::arch::{asm, global_asm};
use core::panic::PanicInfo;
use kernel::machine::{CpuId, MemoryRegion};

#[cfg(feature = "vm-nommu")]
compile_error!("aarch64 boot requires `vm-mmu`");

#[path = "discovery.rs"]
mod discovery;

#[cfg(feature = "selftest")]
#[path = "selftest.rs"]
mod selftest;

global_asm!(include_str!("entry.S"));

// 链接脚本符号：本文档镜像（bootstrap + Core 单一 kaleidos.elf）的物理范围。
// MMU 关着，所以这些地址就是物理地址；Core 把它整段记为永久 Reserved。
unsafe extern "C" {
    static __kernel_start: u8;
    static __kernel_end: u8;
}

/// Read `MPIDR_EL1` (the boot CPU's hardware identity).
#[inline]
fn read_mpidr() -> u64 {
    let value: u64;
    // SAFETY: read-only system register.
    unsafe {
        asm!("mrs {}, mpidr_el1", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// Read `CNTFRQ_EL0` (the generic timer frequency).
#[inline]
fn read_cntfrq() -> u64 {
    let value: u64;
    // SAFETY: read-only system register.
    unsafe {
        asm!("mrs {}, cntfrq_el0", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// Current exception level (for the boot diagnostic line).
#[inline]
fn current_el() -> usize {
    let value: usize;
    // SAFETY: read-only system register.
    unsafe {
        asm!("mrs {}, currentel", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value >> 2
}

/// MPIDR_EL1 → its affinity value, the same shape FDT `/cpus/*/reg` uses.
fn affinity_of(mpidr: u64) -> u64 {
    let (a0, a1, a2, a3) = arch::aarch64::encoding::decode_mpidr_affinity(mpidr);
    arch::aarch64::encoding::encode_affinity(a0, a1, a2, a3)
}

/// QEMU `-kernel` entry.
#[unsafe(no_mangle)]
pub extern "C" fn bootstrap_main(x0: usize) -> ! {
    // Trap state first (vector table), then the CPU-local entry record the
    // Core resolves `current_cpu()` / `per_cpu_base()` through.  Both must be
    // installed before anything can trap or ask "who am I".
    arch::CpuImpl::init_cpu();
    // SAFETY: called on the BSP, interrupts masked, before it goes online.
    unsafe {
        <arch::CpuImpl as CpuArch>::install_per_cpu_base(
            CpuId::from_raw(0),
            core::ptr::NonNull::dangling(),
        );
    }
    arch::aarch64::console::init();

    kernel::log!("bootstrap", "KaleidOS aarch64 bootstrap");
    let boot_affinity = affinity_of(read_mpidr());
    let timebase_frequency = read_cntfrq();
    kernel::log!(
        "bootstrap",
        "EL{} MPIDR={:#x} (affinity {:#x}) CNTFRQ={}Hz",
        current_el(),
        read_mpidr(),
        boot_affinity,
        timebase_frequency
    );

    // SAFETY: the staged buffer holds a verified FDT blob in a static that
    // outlives the parse.
    let dtb_pa = match discovery::stage_boot_dtb(x0) {
        Some(staged) => {
            kernel::log!("bootstrap", "FDT at x0={:#x}, staged at {:#x}", x0, staged);
            staged
        }
        None => panic!("FDT magic: BAD (x0={:#x})", x0),
    };
    let tree = match unsafe { fdt::Fdt::from_ptr_unaligned(dtb_pa as *const u8) } {
        Ok(tree) => {
            kernel::log!("bootstrap", "FDT magic: OK");
            tree
        }
        Err(error) => panic!("FDT parse failed: {:?}", error),
    };
    let dtb_len = discovery::staged_len(dtb_pa);

    // 本文档镜像范围（MMU 关 + identity：链接地址即物理地址）→ reserved
    //（Core 自己，永久保留）。**先**算镜像，再选 arena、再 discovery。
    let image_start = core::ptr::addr_of!(__kernel_start) as usize;
    let image_end = core::ptr::addr_of!(__kernel_end) as usize;
    let image = MemoryRegion {
        base: image_start,
        size: image_end - image_start,
    };
    kernel::log!(
        "bootstrap",
        "image {:#x}-{:#x} ({} KiB)",
        image_start,
        image_end,
        image.size / 1024
    );

    // 无堆早期内存 pass + seam：包含镜像的 bank → 排除镜像与全部 FDT
    // live/reserved 区间后的 arena → `early_init`；此后才允许分配 / 完整
    // discovery（旧流程在镜像边界可知之前就 discovery，这里纠正顺序）。
    let bank = discovery::image_bank(&tree, image_start, image_end)
        .unwrap_or_else(|error| panic!("aarch64 early memory: {}", error));
    let arena = kernel::memory::select_arena(bank, image, |emit| {
        emit(image)?;
        discovery::scan_fdt_exclusions(&tree, dtb_pa, dtb_len, emit)
    })
    .unwrap_or_else(|error| panic!("aarch64 early memory: {}", error));
    kernel::log!(
        "bootstrap",
        "early arena {:#x}-{:#x} ({} KiB), bank {:#x}+{} MiB",
        arena.base,
        arena.base + arena.size,
        arena.size / 1024,
        bank.base,
        bank.size / (1024 * 1024)
    );
    // SAFETY: arena 是 boot 从可用 RAM 中选出的连续窗口——镜像（含 boot 栈 /
    // 内嵌包 / staging buffer）与全部 FDT live/reserved 区间都已被排除；单 CPU、
    // 中断未开、尚无其它分配者。
    if let Err(error) = unsafe { kernel::memory::early_init(arena) } {
        panic!("early memory init failed: {}", error);
    }

    // 完整 discovery（seam 之后）：FDT → core::machine 归一化（owned）。
    let info = discovery::discover(&tree, boot_affinity, timebase_frequency);
    kernel::log!("bootstrap", "MachineInfo dump:");
    kernel::printk!("{:#?}\n", info);

    let reserved = [MemoryRegion {
        base: image_start,
        size: image.size,
    }];
    if let Err(error) = kernel::init(&info, &reserved) {
        panic!("core init failed: {}", error);
    }
    kernel::log!("bootstrap", "BOOT DISCOVERY OK");

    #[cfg(feature = "selftest")]
    {
        crate::selftest::run(&info);
    }

    #[cfg(not(feature = "selftest"))]
    {
        todo!("aarch64 boot: Core Monitor entry (selftest feature only for now)")
    }
}

/// panic 路径的原始写入器：绕过 print 的锁，直连 Console backend。
struct DirectWriter;

impl core::fmt::Write for DirectWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for byte in s.bytes() {
            arch::ConsoleImpl::write_byte(byte);
        }
        Ok(())
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // 组件 panic：先打印一行诊断（直接 console 字节输出，绕过 printk 锁），
    // 再逃逸到 Core 保存的上下文。诊断只有 boot panic handler 拿得到。
    if let Some(escape) = kernel::component::containment::active_escape() {
        let mut writer = DirectWriter;
        let message = info.message();
        let _ = kernel::component::containment::write_escape_line(
            &mut writer,
            escape,
            info.location(),
            Some(&message),
        );
    }
    if kernel::component::panic_escape() {
        loop {
            core::hint::spin_loop();
        }
    }
    let mut writer = DirectWriter;
    let _ = core::fmt::write(&mut writer, format_args!("\nPANIC: {}\n", info));
    loop {
        core::hint::spin_loop();
    }
}
