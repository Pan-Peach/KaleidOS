//! Early-memory seam：无堆 arena 选择 + 一次性分配器启动（Phase 4a）。
//!
//! boot 在**任何分配发生之前**做一次无堆内存 pass：从已校验的 firmware/boot
//! 记录里挑出唯一一段连续、页对齐、已排除所有 live/reserved 区间的 RAM arena，
//! 然后 `memory::early_init(arena)` 把它交给 Core 唯一的分配器。`core::init`
//! 不再初始化 / 重置内存：seam 未跑就直接失败（见 `core::lib::init`）。
//!
//! 本模块只做机制（区间扫描 + 形状校验 + 一次性 gate）；"哪些区间是 live /
//! reserved"是各 arch boot 的策略，由 [`select_arena`] 的 `scan` 闭包提供。
//! 实现**不缓存**排除集、不收集 unbounded 表——每次扫描都重读 firmware 记录。

use super::{ALLOC_GRANULE, HEAP_MIN_ORDER, HEAP_ORDER, align_up_page, init_heap};
use crate::machine::MemoryRegion;
use buddy_system_allocator::{MetadataHeap, PageOrder};
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

/// seam 是否已成功启动过分配器（一次性 gate；`memory::is_initialized` 读它）。
pub(crate) static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// 分配器能真正启动的最小 arena 容量。
///
/// 推导（`MetadataHeap::try_init`）：metadata 从区域前端 carve，每个最小单元
/// （一页）带一份 per-unit metadata；页对齐起点下，一个单元要求
/// `metadata + 一页对齐 + 一页数据` ＝ 2 页。低于此值连一次分配都产生不了，
/// seam 必须 fail-closed。
pub const MIN_ARENA_SIZE: usize = 2 * ALLOC_GRANULE;

pub(crate) fn is_initialized() -> bool {
    INITIALIZED.load(Ordering::Acquire)
}

/// 校验 arena 形状 + 一次性启动（host 测试可传局部 heap / gate，与生产同一路径）。
///
/// # Safety
///
/// 调用方保证 `arena` 满足 [`super::early_init`] 的 SAFETY 契约。
pub(crate) unsafe fn bring_up(
    heap: &Mutex<MetadataHeap<HEAP_ORDER, HEAP_MIN_ORDER>>,
    gate: &AtomicBool,
    arena: MemoryRegion,
) -> Result<(), &'static str> {
    if gate.load(Ordering::Acquire) {
        return Err("memory already initialized");
    }
    let (start, end) = validated_arena(arena)?;

    // SAFETY: 调用方契约（writable、非 live、独占移交）+ 上面的形状校验。
    init_heap(heap, start, end)?;
    // heap 已经 live：无论之后探针结果如何都**不允许**第二次 bring-up 重置它。
    gate.store(true, Ordering::Release);

    let probe = probe_alloc_free(heap)?;
    let probe_end = probe
        .checked_add(ALLOC_GRANULE)
        .ok_or("alloc probe address overflows")?;
    if probe < start || probe_end > end {
        return Err("alloc probe escaped the arena");
    }
    Ok(())
}

/// arena 的形状校验：非零尺寸、端点不溢出、页对齐、不低于最小可用容量。
///
/// 地址 0 也被拒绝：分配器用 `NonNull` 表达块地址，0 不是可移交的 RAM。
pub(crate) fn validated_arena(arena: MemoryRegion) -> Result<(usize, usize), &'static str> {
    if arena.size == 0 {
        return Err("empty arena");
    }
    if arena.base == 0 {
        return Err("arena at physical address zero");
    }
    let end = arena
        .base
        .checked_add(arena.size)
        .ok_or("arena overflows the address space")?;
    if !arena.base.is_multiple_of(ALLOC_GRANULE) || !arena.size.is_multiple_of(ALLOC_GRANULE) {
        return Err("arena is not page aligned");
    }
    if arena.size < MIN_ARENA_SIZE {
        return Err("arena below minimum usable capacity");
    }
    Ok((arena.base, end))
}

/// 分配一帧再释放（沿用既有探针语义），返回该帧基址供 arena canary 检查。
fn probe_alloc_free(
    heap: &Mutex<MetadataHeap<HEAP_ORDER, HEAP_MIN_ORDER>>,
) -> Result<usize, &'static str> {
    let mut heap = heap.lock();
    let run = heap
        .alloc_pages(PageOrder(HEAP_MIN_ORDER as u8))
        .map_err(|_| "alloc probe failed")?;
    let base = run.base.as_ptr() as usize;
    // SAFETY: `run` 刚由同一 heap 分配、尚未释放。
    unsafe { heap.dealloc_pages(run) };
    Ok(base)
}

/// 无堆内存 pass：在 `bank` 中选 **image 之后**最大的页对齐连续空闲间隙，
/// 并列时取最低地址。
///
/// `scan` 每次被调用时把所有必须排除的 live/reserved 区间逐条 `emit` 出去。
/// 实现会对 `scan` 做**重复扫描**（不缓存结果、不收集 unbounded 表）；`emit`
/// 无法解释一条区间时返回 `Err` → 整体 fail-closed（绝不当它不存在）。
///
/// 返回的 arena 保证：页对齐、位于 `bank` 内、不与 `image` 或任何 emit 的区间
/// 相交、起点在 `image` 之后。无可用间隙 / `image` 不在 `bank` 内 / 任何地址
/// 运算溢出 → `Err`。
pub fn select_arena(
    bank: MemoryRegion,
    image: MemoryRegion,
    scan: impl Fn(&mut dyn FnMut(MemoryRegion) -> Result<(), &'static str>) -> Result<(), &'static str>,
) -> Result<MemoryRegion, &'static str> {
    if bank.size == 0 || image.size == 0 {
        return Err("arena scan: empty region");
    }
    let bank_end = bank
        .base
        .checked_add(bank.size)
        .ok_or("arena scan: bank overflows")?;
    let image_end = image
        .base
        .checked_add(image.size)
        .ok_or("arena scan: image overflows")?;
    if image.base < bank.base || image_end > bank_end {
        return Err("arena scan: image outside the bank");
    }

    let mut cursor = align_up_page(image_end);
    let mut best: Option<MemoryRegion> = None;
    while cursor < bank_end {
        // 一次无分配扫描：找出覆盖 `cursor` 的区间（合并覆盖段）与它之后最近
        // 的区间起点。溢出 / 无法解释经 `scan` 的 Result 传播 → fail-closed。
        let mut covered_end = cursor;
        let mut next_start = usize::MAX;
        scan(&mut |excluded| {
            if excluded.size == 0 {
                return Ok(());
            }
            let end = excluded
                .base
                .checked_add(excluded.size)
                .ok_or("arena scan: exclusion overflows")?;
            if excluded.base <= cursor && cursor < end {
                covered_end = covered_end.max(end);
            } else if excluded.base > cursor && excluded.base < next_start {
                next_start = excluded.base;
            }
            Ok(())
        })?;

        if covered_end > cursor {
            cursor = covered_end;
            continue;
        }

        let gap_start = align_up_page(cursor);
        let gap_end = next_start.min(bank_end) & !(ALLOC_GRANULE - 1);
        if gap_start < gap_end {
            let size = gap_end - gap_start;
            // 严格更大才替换：扫描自低向高，并列时保留先发现的（最低地址）。
            let replaces = match best {
                Some(current) => size > current.size,
                None => true,
            };
            if replaces {
                best = Some(MemoryRegion {
                    base: gap_start,
                    size,
                });
            }
        }

        if next_start == usize::MAX {
            break;
        }
        cursor = next_start;
    }

    best.ok_or("arena scan: no usable gap after the image")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::{CpuInfo, HardwareCpuId};

    fn region(base: usize, size: usize) -> MemoryRegion {
        MemoryRegion { base, size }
    }

    /// host backing：std Vec + 页对齐窗口；返回 (keep-alive, arena)。
    fn backing(size: usize) -> (std::vec::Vec<u8>, MemoryRegion) {
        let mut buf = std::vec![0u8; size + ALLOC_GRANULE];
        let pointer = buf.as_mut_ptr() as usize;
        let base = align_up_page(pointer);
        let window = size + ALLOC_GRANULE - (base - pointer);
        (buf, region(base, window & !(ALLOC_GRANULE - 1)))
    }

    // ------------------------------------------------------------------
    // validated_arena：形状（纯逻辑，不碰全局堆）
    // ------------------------------------------------------------------

    #[test]
    fn validated_arena_rejects_zero_overflow_unaligned_and_undersized() {
        assert_eq!(validated_arena(region(0x1000, 0)), Err("empty arena"));
        assert_eq!(
            validated_arena(region(0, 0x4000)),
            Err("arena at physical address zero")
        );
        assert_eq!(
            validated_arena(region(usize::MAX - 0x1fff, 0x4000)),
            Err("arena overflows the address space")
        );
        assert_eq!(
            validated_arena(region(0x1001, 0x4000)),
            Err("arena is not page aligned")
        );
        assert_eq!(
            validated_arena(region(0x1000, 0x4001)),
            Err("arena is not page aligned")
        );
        assert_eq!(
            validated_arena(region(0x1000, MIN_ARENA_SIZE - ALLOC_GRANULE)),
            Err("arena below minimum usable capacity")
        );
        assert_eq!(
            validated_arena(region(0x1000, MIN_ARENA_SIZE)),
            Ok((0x1000, 0x1000 + MIN_ARENA_SIZE))
        );
    }

    // ------------------------------------------------------------------
    // select_arena：选择规则 + canary + fail-closed
    // ------------------------------------------------------------------

    /// 64 KiB bank、8 KiB image、两条排除区间。image 之后有两个间隙：
    /// [0x4000,0x8000) = 16 KiB 与 [0x9000,0x11000) = 32 KiB → 取后者。
    #[test]
    fn select_arena_picks_largest_gap_after_image() {
        let bank = region(0x1000, 0x10000);
        let image = region(0x1000, 0x2000);
        let fdt = region(0x3000, 0x1000);
        let reserved = region(0x8000, 0x1000);
        let arena = select_arena(bank, image, |emit| {
            emit(image)?;
            emit(fdt)?;
            emit(reserved)?;
            Ok(())
        })
        .expect("arena");
        assert_eq!(arena, region(0x9000, 0x8000));

        // canary：arena 与镜像 / 每条排除区间不相交（含未对齐边界的相交检查）。
        for excluded in [image, fdt, reserved] {
            let excluded_end = excluded.base + excluded.size;
            let arena_end = arena.base + arena.size;
            assert!(
                arena.base >= excluded_end || arena_end <= excluded.base,
                "arena {arena:?} overlaps {excluded:?}"
            );
        }
        assert!(arena.base >= image.base + image.size, "must be after image");
        assert!(arena.base.is_multiple_of(ALLOC_GRANULE));
        assert!(arena.size.is_multiple_of(ALLOC_GRANULE));
    }

    /// 并列取最低地址：两个等大间隙。
    #[test]
    fn select_arena_breaks_size_ties_by_lowest_address() {
        let bank = region(0x10000, 0x20000);
        let image = region(0x10000, 0x1000);
        let middle = region(0x20000, 0x1000); // [0x20000, 0x21000)
        let arena = select_arena(bank, image, |emit| {
            emit(image)?;
            emit(middle)?;
            Ok(())
        })
        .expect("arena");
        // 间隙 A = [0x11000, 0x20000) = 0xf000；间隙 B = [0x21000, 0x30000) = 0xf000
        // 等大 → 取最低地址 A。
        assert_eq!(arena, region(0x11000, 0xf000));

        // 真正等大的两半：exclusion 恰好在正中间。
        let bank = region(0x1000, 0x10000);
        let image = region(0x1000, 0x1000);
        let middle = region(0x9000, 0x1000);
        let arena = select_arena(bank, image, |emit| {
            emit(image)?;
            emit(middle)?;
            Ok(())
        })
        .expect("arena");
        assert_eq!(arena, region(0x2000, 0x7000), "first (lowest) tie wins");
    }

    /// 未对齐的排除区间不会产出未对齐的 arena，也不会被"跳过"。
    #[test]
    fn select_arena_aligns_gaps_around_unaligned_exclusions() {
        let bank = region(0x1000, 0x8000);
        let image = region(0x1000, 0x1000);
        // 未对齐区间 [0x2501, 0x2a03)：之后最大的空闲从 0x3000 起。
        let odd = region(0x2501, 0x502);
        let arena = select_arena(bank, image, |emit| {
            emit(image)?;
            emit(odd)?;
            Ok(())
        })
        .expect("arena");
        assert_eq!(arena, region(0x3000, 0x6000));
        assert!(arena.base.is_multiple_of(ALLOC_GRANULE));
        assert!(arena.size.is_multiple_of(ALLOC_GRANULE));
    }

    /// 排除区间的端点未对齐时，子页大小的"假间隙"必须被跳过（不得下溢 / 回绕）。
    #[test]
    fn select_arena_skips_sub_page_gaps_from_unaligned_exclusions() {
        let bank = region(0x1000, 0x8000);
        let image = region(0x1000, 0x501);
        // A 覆盖 cursor（0x2000）且末端未对齐 → cursor = 0x2203；
        // B 起点 0x2a03 对齐到页后 < gap_start（0x3000）→ 假间隙，必须跳过。
        let a = region(0x1800, 0xa03);
        let b = region(0x2a03, 0x600);
        let arena = select_arena(bank, image, |emit| {
            emit(image)?;
            emit(a)?;
            emit(b)?;
            Ok(())
        })
        .expect("arena");
        // 真间隙是 B 之后：[0x4000, 0x9000)。
        assert_eq!(arena, region(0x4000, 0x5000));
    }

    /// 无排除区间：取 image 末尾到 bank 末尾，页对齐。
    #[test]
    fn select_arena_without_exclusions_uses_rest_of_bank() {
        let bank = region(0x20000, 0x9000);
        let image = region(0x20000, 0x1001);
        let arena = select_arena(bank, image, |_emit| Ok(())).expect("arena");
        assert_eq!(arena, region(0x22000, 0x7000));
    }

    #[test]
    fn select_arena_fails_when_image_is_outside_the_bank() {
        let bank = region(0x10000, 0x10000);
        let image = region(0x0, 0x1000);
        assert_eq!(
            select_arena(bank, image, |emit| {
                emit(image)?;
                Ok(())
            }),
            Err("arena scan: image outside the bank")
        );
        let image = region(0x10000, 0x10001);
        assert_eq!(
            select_arena(bank, image, |_emit| Ok(())),
            Err("arena scan: image outside the bank")
        );
    }

    /// 无间隙：image 贴着 bank 末尾，或被排除区间完全覆盖。
    #[test]
    fn select_arena_fails_when_no_gap_remains() {
        let bank = region(0x1000, 0x4000);
        let image = region(0x1000, 0x3ffe);
        assert_eq!(
            select_arena(bank, image, |_emit| Ok(())),
            Err("arena scan: no usable gap after the image")
        );

        let bank = region(0x1000, 0x4000);
        let image = region(0x1000, 0x1000);
        let covers = region(0x2000, 0x3000);
        assert_eq!(
            select_arena(bank, image, |emit| {
                emit(covers)?;
                Ok(())
            }),
            Err("arena scan: no usable gap after the image")
        );
    }

    /// 无法解释的 reservation：scan 返回 Err → fail-closed，绝不忽略。
    #[test]
    fn select_arena_fails_closed_when_scan_rejects() {
        let bank = region(0x1000, 0x10000);
        let image = region(0x1000, 0x1000);
        assert_eq!(
            select_arena(bank, image, |_emit| Err("uninterpretable reservation")),
            Err("uninterpretable reservation")
        );
    }

    /// 排除区间自身溢出 → fail-closed。
    #[test]
    fn select_arena_fails_closed_on_exclusion_overflow() {
        let bank = region(0x1000, 0x10000);
        let image = region(0x1000, 0x1000);
        assert_eq!(
            select_arena(bank, image, |emit| {
                emit(region(usize::MAX - 0x10, 0x20))?;
                Ok(())
            }),
            Err("arena scan: exclusion overflows")
        );
    }

    // ------------------------------------------------------------------
    // bring_up：一次性 gate + 探针 canary（局部 heap / gate）
    // ------------------------------------------------------------------

    #[test]
    fn bring_up_runs_once_and_rejects_a_second_call() {
        let (_keep, arena) = backing(1 << 16);
        let heap = Mutex::new(MetadataHeap::<HEAP_ORDER, HEAP_MIN_ORDER>::empty());
        let gate = AtomicBool::new(false);

        // SAFETY: arena 是测试进程里独占、可写的连续内存窗口。
        unsafe { bring_up(&heap, &gate, arena) }.expect("first bring-up");
        assert!(gate.load(Ordering::Acquire), "seam gate must be set");
        assert!(heap.lock().stats_total_bytes() > 0, "heap must be live");

        // 第二次调用必须在触碰 heap 之前拒绝（重置 live heap = 破坏真相）。
        let before = heap.lock().stats_total_bytes();
        // SAFETY: 契约同第一次；gate 拒绝路径不触碰 arena。
        assert_eq!(
            unsafe { bring_up(&heap, &gate, arena) },
            Err("memory already initialized")
        );
        assert_eq!(
            heap.lock().stats_total_bytes(),
            before,
            "second call must not re-initialize the heap"
        );
    }

    #[test]
    fn bring_up_rejects_invalid_arena_without_touching_the_heap() {
        let (_keep, good) = backing(1 << 16);
        let heap = Mutex::new(MetadataHeap::<HEAP_ORDER, HEAP_MIN_ORDER>::empty());
        let gate = AtomicBool::new(false);

        for bad in [
            region(good.base, 0),
            region(good.base, ALLOC_GRANULE),
            region(good.base + 1, 1 << 16),
            region(usize::MAX - 0x1000, 0x2000),
        ] {
            // SAFETY: 非法 arena 在校验阶段被拒，不触碰任何内存。
            assert!(unsafe { bring_up(&heap, &gate, bad) }.is_err(), "{bad:?}");
        }
        assert_eq!(heap.lock().stats_total_bytes(), 0, "heap must stay empty");
        assert!(!gate.load(Ordering::Acquire));

        // SAFETY: 合法路径仍然可用（gate 未被失败污染）。
        unsafe { bring_up(&heap, &gate, good) }.expect("valid arena must succeed");
    }

    // ------------------------------------------------------------------
    // core::init 的 seam 前置条件（(c)：seam 没跑必须拒绝）
    // ------------------------------------------------------------------

    /// host 测试进程里全局 gate 恒为 false（fixture 直接 `try_init` 全局堆、
    /// 不调用 `early_init`），因此 `core::init` 必须在碰任何全局状态之前拒绝。
    #[test]
    fn core_init_fails_when_the_seam_did_not_run() {
        assert!(
            !crate::memory::is_initialized(),
            "host fixtures must not trip the seam gate"
        );
        let info = crate::machine::test_support::snapshot(
            HardwareCpuId::from_raw(0),
            0,
            alloc::vec![CpuInfo {
                boot_cpu: true,
                hardware_id: HardwareCpuId::from_raw(0),
            }],
            alloc::vec![region(0x8000_0000, 1 << 20)],
            alloc::vec![],
        );
        assert_eq!(
            crate::init(info, &[]).err(),
            Some("memory not early-initialized"),
            "core::init must fail-closed before using the allocator"
        );
    }
}
