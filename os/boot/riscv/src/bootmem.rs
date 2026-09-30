//! RV64 / RV32 共用的**无堆早期内存 pass**（Phase 4a）。
//!
//! 在分配器存在之前，boot 从已校验的 FDT 记录里回答两个问题：
//! 1. 哪个可用 RAM bank 包含已加载镜像（`image_bank`，找不到即失败，绝不落回
//!    无关 bank）；
//! 2. 除镜像之外还有哪些 live/reserved 区间必须从 arena 里排除
//!    （`scan_fdt_exclusions`：保留的 FDT 整体、FDT header 的 memory
//!    reservation map、`/reserved-memory` 的定址子节点）。
//!
//! 本模块不做分配、不缓存排除集；`scan_fdt_exclusions` 会被
//! `kernel::memory::select_arena` 反复调用（每次重读 FDT 记录）。无法解释的
//! reservation 一律返回 `Err` → fail-closed。

use kernel::machine::MemoryRegion;

type FdtParser<'a> = (
    fdt::parsing::unaligned::UnalignedParser<'a>,
    fdt::parsing::Panic,
);

/// 定位完全覆盖 `[kernel_pa, kernel_pa + image_size)` 的 RAM bank。
pub fn image_bank<'a>(
    tree: &fdt::Fdt<'a, FdtParser<'a>>,
    kernel_pa: usize,
    image_size: usize,
) -> Result<MemoryRegion, &'static str> {
    let image_end = kernel_pa
        .checked_add(image_size)
        .ok_or("image range overflows")?;
    let Some(memory) = tree.find_node("/memory") else {
        return Err("FDT has no /memory node");
    };
    let Some(regions) = memory.reg() else {
        return Err("FDT /memory has no reg");
    };
    for entry in regions.iter::<u64, u64>() {
        let Ok(entry) = entry else { continue };
        // u64 → usize：32 位目标容不下的区间在目标地址空间之外，不可能包含镜像。
        let (Ok(base), Ok(size)) = (usize::try_from(entry.address), usize::try_from(entry.len))
        else {
            continue;
        };
        let Some(end) = base.checked_add(size) else {
            continue;
        };
        if base <= kernel_pa && image_end <= end {
            return Ok(MemoryRegion { base, size });
        }
    }
    Err("no RAM bank contains the loaded image")
}

/// Core 堆的 KernelNative 组件镜像按 PC-relative 重定位到镜像符号
/// （RISC-V CALL/PCREL 的 ±2 GiB 窗口），组件镜像只能落在
/// `[bank.base, image.base + 2 GiB)` 内。
///
/// 收窄 arena 搜索窗口到该范围：QEMU virt `-m 4G` 上 FDT 恰好把 RAM 切成
/// "镜像之后"与"FDT 之后"两段，后者按字节更大却越过可达窗口——直接选它会让
/// `kcomp_smoke` 装载以 `UnsupportedRelocation` 回归。窗口收窄后仍是窗口内的
/// 最大间隙，Core 的物理 inventory（`MachineInfo.memory_regions`）不受影响。
pub fn arena_search_window(bank: MemoryRegion, image: MemoryRegion) -> MemoryRegion {
    let bank_end = bank.base.saturating_add(bank.size);
    let reach_end = image.base.saturating_add(1usize << 31);
    let end = bank_end.min(reach_end);
    MemoryRegion {
        base: bank.base,
        size: end.saturating_sub(bank.base),
    }
}

/// 逐条 emit 所有 **FDT 派生**的 live/reserved 区间（不含镜像本身）。
///
/// 覆盖：保留的 FDT 整体 `[dtb_pa, dtb_pa + totalsize)`、header memory
/// reservation map 的每个条目、`/reserved-memory` 每个子节点的 `reg`。
/// 任何无法解释的形状（无终结符、越界、无 `reg` 的子节点）→ `Err`。
pub fn scan_fdt_exclusions<'a>(
    tree: &fdt::Fdt<'a, FdtParser<'a>>,
    dtb_pa: usize,
    emit: &mut dyn FnMut(MemoryRegion) -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    // 保留的 FDT 整体：boot 在完整 discovery 结束前持续读它。
    let total_size = tree.total_size();
    if total_size == 0 {
        return Err("FDT totalsize is zero");
    }
    emit(MemoryRegion {
        base: dtb_pa,
        size: total_size,
    })?;

    // Header memory reservation map（16 字节零对终结；全程 bounded）。
    let start = tree.header().memory_reserve_map_offset as usize;
    let mut cursor = start;
    loop {
        let end = cursor
            .checked_add(16)
            .ok_or("FDT reservation entry overflows")?;
        if end > total_size {
            return Err("FDT reservation block runs past totalsize");
        }
        let base = read_be_u64(dtb_pa + cursor);
        let size = read_be_u64(dtb_pa + cursor + 8);
        if base == 0 && size == 0 {
            break;
        }
        emit_reservation(base, size, emit)?;
        cursor = end;
    }

    // /reserved-memory：只支持定址（`reg`）的子节点；其它形状 fail-closed。
    if let Some(node) = tree.find_node("/reserved-memory") {
        for child in node.children() {
            let Some(reg) = child.reg() else {
                return Err("uninterpretable /reserved-memory child (no reg)");
            };
            for entry in reg.iter::<u64, u64>() {
                let entry = entry.map_err(|_| "uninterpretable /reserved-memory reg entry")?;
                emit_reservation(entry.address, entry.len, emit)?;
            }
        }
    }
    Ok(())
}

/// FDT 里的 (address, size) 是 u64；转换到目标地址宽度。
///
/// - `size == 0`：空 reservation，跳过；
/// - `address` 超出目标宽度：整个区间在目标地址空间之外，不可能与 arena 相交，
///   跳过（32 位目标上的 64 位区间）；
/// - `size` 超出目标宽度：区间起点可达但长度无法表达 → fail-closed。
fn emit_reservation(
    base: u64,
    size: u64,
    emit: &mut dyn FnMut(MemoryRegion) -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    if size == 0 {
        return Ok(());
    }
    let Ok(base) = usize::try_from(base) else {
        return Ok(());
    };
    let size = usize::try_from(size).map_err(|_| "FDT reservation does not fit the target")?;
    emit(MemoryRegion { base, size })
}

/// Big-endian u64 的逐字节 volatile 读（DTB 可能未对齐，不假设对齐 / 不构造引用）。
fn read_be_u64(pa: usize) -> u64 {
    let mut bytes = [0u8; 8];
    for (offset, byte) in bytes.iter_mut().enumerate() {
        // SAFETY: 物理 identity 映射下的 raw 读；见函数文档。
        *byte = unsafe { core::ptr::read_volatile((pa + offset) as *const u8) };
    }
    u64::from_be_bytes(bytes)
}
