//! 私有域堆后端（C 实现）+ KernelNative `GlobalAlloc` adapter 的 host 测试。
//!
//! - [`Heap`] facade 直接走**真实 C 实现**（build.rs 编出的 `libkalloc.a`），不是
//!   Rust 复刻。覆盖：split / coalesce、对齐（含 > 16）、溢出、OOM、realloc 语义
//!   （保留内容 / 失败保旧块）、backing 增长路径、两个独立堆互不干扰。
//! - adapter（[`KernelHeap`]）走 `test_support` 的 `kcore_heap_alloc/dealloc` 替身，
//!   钉死 ABI 契约：原始 layout 逐字传递、realloc = alloc + copy + dealloc。

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::alloc::KernelHeap;
use crate::heap::Heap;

/// 4096 对齐的静态 arena（分配器会写它 → 必须 UnsafeCell）。
#[repr(C, align(4096))]
struct Arena<const N: usize>(UnsafeCell<[u8; N]>);

// SAFETY: 每个 arena 静态只被一个测试使用；测试内串行访问。
unsafe impl<const N: usize> Sync for Arena<N> {}

impl<const N: usize> Arena<N> {
    const fn new() -> Self {
        Self(UnsafeCell::new([0u8; N]))
    }

    fn base(&self) -> *mut u8 {
        self.0.get().cast::<u8>()
    }
}

macro_rules! arena {
    ($name:ident, $n:expr) => {
        static $name: Arena<$n> = Arena::new();
    };
}

/// 测试 backing：从静态池按 `min_align` 切 region，记录调用次数 / 最近 `min_len`。
/// 行为与 `kcore_memory_acquire` 同形（成功 0 并写 out；失败 `-ENOMEM`）。
#[repr(C, align(4096))]
struct Pool<const N: usize> {
    buf: UnsafeCell<[u8; N]>,
    cursor: AtomicUsize,
    calls: AtomicUsize,
    last_min_len: AtomicUsize,
    fail: bool,
}

// SAFETY: 每个池静态只被一个测试使用。
unsafe impl<const N: usize> Sync for Pool<N> {}

impl<const N: usize> Pool<N> {
    const fn new(fail: bool) -> Self {
        Self {
            buf: UnsafeCell::new([0u8; N]),
            cursor: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
            last_min_len: AtomicUsize::new(0),
            fail,
        }
    }

    /// # Safety
    /// `out_base` / `out_len` 必须是可写输出位置（backing 契约）。
    unsafe fn acquire(
        &self,
        min_len: usize,
        min_align: usize,
        out_base: *mut usize,
        out_len: *mut usize,
    ) -> i32 {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.last_min_len.store(min_len, Ordering::Relaxed);
        if self.fail {
            return -12; // -ENOMEM
        }
        let base = self.buf.get().cast::<u8>() as usize;
        let align = min_align.max(8);
        let start = (base + self.cursor.load(Ordering::Relaxed) + align - 1) & !(align - 1);
        let used = start - base;
        if N - used < min_len {
            return -12;
        }
        self.cursor.store(used + min_len, Ordering::Relaxed);
        // SAFETY: 由调用方保证 out 可写（见 # Safety）。
        unsafe {
            *out_base = start;
            *out_len = min_len;
        }
        0
    }
}

macro_rules! backing_fn {
    ($fname:ident, $pool:ident, $cap:expr, $fail:expr) => {
        static $pool: Pool<$cap> = Pool::new($fail);

        unsafe extern "C" fn $fname(
            min_len: usize,
            min_align: usize,
            out_base: *mut usize,
            out_len: *mut usize,
        ) -> i32 {
            // SAFETY: 测试 backing 的 out 契约与 kcore_memory_acquire 一致。
            unsafe { $pool.acquire(min_len, min_align, out_base, out_len) }
        }
    };
}

// ---------------------------------------------------------------------------
// place / split / coalesce
// ---------------------------------------------------------------------------

arena!(ARENA_PLACE, 8192);
backing_fn!(backing_place, POOL_PLACE, 4096, false);

#[test]
fn heap_place_returns_region_base() {
    let base = ARENA_PLACE.base();
    // SAFETY: arena 独占、4096 对齐、8192 字节；backing 合法。
    let heap = unsafe { Heap::place(base, 8192, backing_place) };
    assert!(!heap.is_null());
    assert_eq!(heap.cast::<u8>(), base);
}

arena!(ARENA_SPLIT, 8192);
backing_fn!(backing_split, POOL_SPLIT, 4096, false);

#[test]
fn heap_alloc_splits_and_free_coalesces() {
    // SAFETY: 独占 arena；句柄/指针契约见 heap 模块。
    let heap = unsafe { Heap::place(ARENA_SPLIT.base(), 8192, backing_split) };
    assert!(!heap.is_null());

    let p1 = unsafe { (*heap).alloc(64, 8) };
    let p2 = unsafe { (*heap).alloc(64, 8) };
    let p3 = unsafe { (*heap).alloc(64, 8) };
    assert!(!p1.is_null() && !p2.is_null() && !p3.is_null());
    assert!(p1 < p2 && p2 < p3, "first-fit 从低地址开始切分");

    unsafe {
        (*heap).free(p2);
        (*heap).free(p1);
    }
    // p1 + p2 合并（176B）后，120B 的分配应复用 p1 的地址（split 证据）。
    let p4 = unsafe { (*heap).alloc(120, 8) };
    assert_eq!(p4, p1, "合并出的空洞应被复用");

    unsafe {
        (*heap).free(p3);
        (*heap).free(p4);
    }
    // 全部归还后整段 region 重新可用（coalesce 证据）。
    let big = unsafe { (*heap).alloc(7900, 8) };
    assert!(!big.is_null(), "coalesce 后整段 region 应可用");
    unsafe { (*heap).free(big) };
}

// ---------------------------------------------------------------------------
// 对齐 / 溢出 / OOM
// ---------------------------------------------------------------------------

arena!(ARENA_ALIGN, 65536);
backing_fn!(backing_align, POOL_ALIGN, 4096, false);

#[test]
fn heap_alloc_respects_alignment() {
    let heap = unsafe { Heap::place(ARENA_ALIGN.base(), 65536, backing_align) };
    assert!(!heap.is_null());
    let cases = [
        (1usize, 1usize),
        (8, 8),
        (16, 16),
        (33, 32),
        (64, 64),
        (100, 256),
        (4096, 4096),
    ];
    for (size, align) in cases {
        let p = unsafe { (*heap).alloc(size, align) };
        assert!(!p.is_null(), "alloc(size={size}, align={align}) failed");
        assert_eq!(p as usize % align, 0, "align={align} 未满足");
        // 内存确实可写（写满请求的 size 字节，验证块容量足够）。
        unsafe { core::ptr::write_bytes(p, 0xA5, size) };
    }
}

arena!(ARENA_OVERFLOW, 4096);
backing_fn!(backing_overflow, POOL_OVERFLOW, 4096, true);

#[test]
fn heap_alloc_rejects_overflow_and_bad_align() {
    let heap = unsafe { Heap::place(ARENA_OVERFLOW.base(), 4096, backing_overflow) };
    assert!(!heap.is_null());
    let null = core::ptr::null_mut::<u8>();
    // SAFETY: 句柄有效；这些入参必须被入口检查拒绝（不 panic、不向 backing 要内存）。
    unsafe {
        assert_eq!((*heap).alloc(usize::MAX, 8), null);
        assert_eq!((*heap).alloc(usize::MAX - 7, 8), null);
        assert_eq!((*heap).alloc(16, 3), null, "非 2 的幂 align");
        assert_eq!((*heap).alloc(16, 0), null, "align = 0");
        assert_eq!((*heap).alloc(0, 8), null, "size = 0");
    }
    assert_eq!(
        POOL_OVERFLOW.calls.load(Ordering::Relaxed),
        0,
        "入口拒绝不得触发 backing"
    );
    // 巨大 align：放不下 → null（不是 panic / 不是溢出 UB）。
    let huge = unsafe { (*heap).alloc(1, 1usize << (usize::BITS - 1)) };
    assert_eq!(huge, null);
}

arena!(ARENA_OOM, 512);
backing_fn!(backing_oom, POOL_OOM, 4096, true);

#[test]
fn heap_alloc_returns_null_when_backing_exhausted() {
    let heap = unsafe { Heap::place(ARENA_OOM.base(), 512, backing_oom) };
    assert!(!heap.is_null());
    let ok = unsafe { (*heap).alloc(64, 8) };
    assert!(!ok.is_null());
    unsafe { ok.write(0x5A) };
    // 放不下 + backing 失败 → null；已有块不受影响。
    assert_eq!(unsafe { (*heap).alloc(4096, 8) }, core::ptr::null_mut());
    assert_eq!(unsafe { ok.read() }, 0x5A);
    // 失败后堆仍可用：释放再分配。
    unsafe { (*heap).free(ok) };
    let again = unsafe { (*heap).alloc(200, 8) };
    assert!(!again.is_null());
    unsafe { (*heap).free(again) };
}

// ---------------------------------------------------------------------------
// realloc / 增长
// ---------------------------------------------------------------------------

arena!(ARENA_REALLOC, 8192);
backing_fn!(backing_realloc, POOL_REALLOC, 4096, false);

#[test]
fn heap_realloc_preserves_contents() {
    let heap = unsafe { Heap::place(ARENA_REALLOC.base(), 8192, backing_realloc) };
    assert!(!heap.is_null());
    let p = unsafe { (*heap).alloc(100, 8) };
    assert!(!p.is_null());
    for i in 0..100 {
        unsafe { p.add(i).write(i as u8) };
    }
    let grown = unsafe { (*heap).realloc(p, 4000, 8) };
    assert!(!grown.is_null());
    for i in 0..100 {
        assert_eq!(
            unsafe { grown.add(i).read() },
            i as u8,
            "realloc 必须保留前缀内容"
        );
    }
    let shrunk = unsafe { (*heap).realloc(grown, 50, 8) };
    assert!(!shrunk.is_null());
    for i in 0..50 {
        assert_eq!(unsafe { shrunk.add(i).read() }, i as u8);
    }
    unsafe { (*heap).free(shrunk) };
}

arena!(ARENA_REALLOC_FAIL, 1024);
backing_fn!(backing_realloc_fail, POOL_REALLOC_FAIL, 4096, true);

#[test]
fn heap_realloc_failure_keeps_old_block() {
    let heap = unsafe { Heap::place(ARENA_REALLOC_FAIL.base(), 1024, backing_realloc_fail) };
    assert!(!heap.is_null());
    let p = unsafe { (*heap).alloc(64, 8) };
    assert!(!p.is_null());
    for i in 0..64 {
        unsafe { p.add(i).write(0x5A ^ i as u8) };
    }
    let failed = unsafe { (*heap).realloc(p, 8192, 8) };
    assert_eq!(
        failed,
        core::ptr::null_mut(),
        "backing 失败 → realloc 返回 null"
    );
    for i in 0..64 {
        assert_eq!(
            unsafe { p.add(i).read() },
            0x5A ^ i as u8,
            "失败时旧块必须原样保留"
        );
    }
    // 旧块仍可正常释放，堆仍可用。
    unsafe { (*heap).free(p) };
    let again = unsafe { (*heap).alloc(128, 8) };
    assert!(!again.is_null());
    unsafe { (*heap).free(again) };
}

arena!(ARENA_GROW, 256);
backing_fn!(backing_grow, POOL_GROW, 65536, false);

#[test]
fn heap_growth_calls_backing_geometrically() {
    // 初始 region 只够一次 200B 分配。
    let heap = unsafe { Heap::place(ARENA_GROW.base(), 256, backing_grow) };
    assert!(!heap.is_null());
    let a = unsafe { (*heap).alloc(200, 8) };
    assert!(!a.is_null());
    assert_eq!(
        POOL_GROW.calls.load(Ordering::Relaxed),
        0,
        "初始 region 够用 → 不打扰 backing"
    );
    let b = unsafe { (*heap).alloc(200, 8) };
    assert!(!b.is_null());
    assert_ne!(a, b);
    assert_eq!(
        POOL_GROW.calls.load(Ordering::Relaxed),
        1,
        "放不下 → 恰好增长一次"
    );
    assert!(
        POOL_GROW.last_min_len.load(Ordering::Relaxed) >= 4096,
        "增长是几何式请求容量（起步 4 KiB），不是精确 need"
    );
}

// ---------------------------------------------------------------------------
// KernelNative GlobalAlloc adapter（Core 共享堆 ABI：kcore_heap_alloc/dealloc）
// ---------------------------------------------------------------------------

/// alloc/dealloc/realloc 都经 Core 堆 ABI；dealloc 必须拿**原始 layout**，
/// realloc = alloc + copy + dealloc 旧块（不是把取整容量传回去）。
#[test]
fn global_alloc_adapter_roundtrips_through_core_heap_abi() {
    let _guard = crate::test_support::lock();
    crate::test_support::reset_script();
    let adapter = KernelHeap;
    let layout = Layout::from_size_align(32, 8).unwrap();

    // alloc：size / align 原样进入 Core ABI。
    let p = unsafe { adapter.alloc(layout) };
    assert!(!p.is_null());
    assert_eq!(
        crate::test_support::last_heap_alloc(),
        Some(crate::test_support::HeapAllocRecord { size: 32, align: 8 })
    );
    for i in 0..32 {
        unsafe { p.add(i).write(i as u8) };
    }

    // realloc：新块走 Core alloc，旧块按**原始 layout** 归还，前缀内容保留。
    let q = unsafe { adapter.realloc(p, layout, 128) };
    assert!(!q.is_null());
    for i in 0..32 {
        assert_eq!(unsafe { q.add(i).read() }, i as u8);
    }
    assert_eq!(
        crate::test_support::last_heap_dealloc(),
        Some(crate::test_support::HeapDeallocRecord {
            ptr: p as usize,
            size: 32,
            align: 8,
        }),
        "realloc 必须用旧 layout 归还旧块"
    );

    // dealloc：与前一次成功 alloc 逐字一致。
    unsafe { adapter.dealloc(q, Layout::from_size_align(128, 8).unwrap()) };
    assert_eq!(
        crate::test_support::last_heap_dealloc(),
        Some(crate::test_support::HeapDeallocRecord {
            ptr: q as usize,
            size: 128,
            align: 8,
        })
    );
}

/// Core 堆耗尽 → alloc 返回 null、realloc 返回 null 且**旧块原样保留**
/// （GlobalAlloc 契约；adapter 不 panic、不擅自释放）。
#[test]
fn global_alloc_adapter_surfaces_core_exhaustion_as_null() {
    let _guard = crate::test_support::lock();
    crate::test_support::reset_script();
    let adapter = KernelHeap;
    let layout = Layout::from_size_align(16, 8).unwrap();

    crate::test_support::script_heap_exhaustion();
    assert!(unsafe { adapter.alloc(layout) }.is_null());

    crate::test_support::reset_script();
    let p = unsafe { adapter.alloc(layout) };
    assert!(!p.is_null());
    unsafe { p.write(0x5A) };

    crate::test_support::script_heap_exhaustion();
    assert!(unsafe { adapter.realloc(p, layout, 64) }.is_null());
    assert_eq!(unsafe { p.read() }, 0x5A, "realloc 失败必须保留旧块");

    // 清理：耗尽脚本已消费（非 0 值在下次 alloc 仍会触发，先复位再释放）。
    crate::test_support::reset_script();
    unsafe { adapter.dealloc(p, layout) };
}

// ---------------------------------------------------------------------------
// 两个独立堆互不干扰
// ---------------------------------------------------------------------------

arena!(ARENA_TWO_A, 4096);
arena!(ARENA_TWO_B, 4096);
backing_fn!(backing_two_a, POOL_TWO_A, 4096, false);
backing_fn!(backing_two_b, POOL_TWO_B, 4096, false);

#[test]
fn independent_heaps_do_not_interfere() {
    let heap_a = unsafe { Heap::place(ARENA_TWO_A.base(), 4096, backing_two_a) };
    let heap_b = unsafe { Heap::place(ARENA_TWO_B.base(), 4096, backing_two_b) };
    assert!(!heap_a.is_null() && !heap_b.is_null() && heap_a != heap_b);

    let a = unsafe { (*heap_a).alloc(128, 8) };
    let b = unsafe { (*heap_b).alloc(128, 8) };
    assert!(!a.is_null() && !b.is_null() && a != b);
    for i in 0..128 {
        unsafe {
            a.add(i).write(0xAA);
            b.add(i).write(0xBB);
        }
    }
    unsafe { (*heap_a).free(a) };
    for i in 0..128 {
        assert_eq!(
            unsafe { b.add(i).read() },
            0xBB,
            "A 堆的 free 不得影响 B 堆"
        );
    }
    let a2 = unsafe { (*heap_a).alloc(128, 8) };
    assert_eq!(a2, a, "A 堆应复用自己释放的块");
    let b2 = unsafe { (*heap_b).alloc(64, 8) };
    assert!(!b2.is_null(), "B 堆独立分配");
    unsafe {
        (*heap_a).free(a2);
        (*heap_b).free(b2);
        (*heap_b).free(b);
    }
}
