//! Host 测试专用页池（`cfg(test)`）：给 sv39/sv32 动态页表 walk 提供
//! identity backing，不改任何生产结构。
//!
//! 原理：Sv39PageTable/Sv32PageTable 通过 `PageAlloc` 回调拿"一个已归零页的
//! 物理地址"，然后按 `(ppn << 12) as *mut` 解引用（v1 identity 阶段契约）。
//! host 测试用 mmap 一块区域扮演这些页：
//! - Sv39：任意地址即可（x86_64/aarch64 用户地址 < 2^47 << 2^56，PPN 44 位不截断）；
//! - Sv32：PTE 只有 22 位 PPN（真实地址 < 2^34 才不被截断），必须 mmap 到
//!   0x8000_0000 附近的低地址 —— 仅 Linux 可（macOS 用户区起点高于此）。
//!
//! 并发：`guard()` 提供测试级互斥（独立于分配状态锁），cargo test 并行线程下安全。
//! 分配状态：`init()` 重置，`set_fail_after(n)` 让第 n 次分配开始失败（测回滚）。

#![cfg(test)]

use std::sync::Mutex;

const PAGE: usize = 4096;
const PROT_READ: i32 = 1;
const PROT_WRITE: i32 = 2;
const MAP_PRIVATE: i32 = 2;
const MAP_ANONYMOUS: i32 = 0x20;
const MAP_FIXED: i32 = 0x10;

unsafe extern "C" {
    fn mmap(
        addr: *mut core::ffi::c_void,
        len: usize,
        prot: i32,
        flags: i32,
        fd: i32,
        off: i64,
    ) -> *mut core::ffi::c_void;
}

struct PoolState {
    base: usize,
    pages: usize,
    next: usize,
    fail_after: usize,
}

static STATE: Mutex<PoolState> = Mutex::new(PoolState {
    base: 0,
    pages: 0,
    next: 0,
    fail_after: usize::MAX,
});

/// 测试互斥：同一时间只允许一个使用页池的测试（与状态锁分离，避免自锁）。
static GUARD: Mutex<()> = Mutex::new(());

pub(crate) fn guard() -> std::sync::MutexGuard<'static, ()> {
    // 容忍先前的测试 panic 留下的毒（并行测试隔离优先于毒化保护）
    GUARD.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// 映射 `pages` 个连续零页。`base` 为 0 时让内核挑地址（Sv39）；
/// `fixed` 要求精确落在 `base`（Sv32 的低地址约束）。
pub(crate) fn init(base: usize, pages: usize, fixed: bool) {
    assert!(pages > 0, "pool needs at least one page");
    let mut state = STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    let len = pages * PAGE;
    let flags = MAP_PRIVATE | MAP_ANONYMOUS | if fixed { MAP_FIXED } else { 0 };
    let addr = unsafe {
        mmap(
            base as *mut core::ffi::c_void,
            len,
            PROT_READ | PROT_WRITE,
            flags,
            -1,
            0,
        )
    };
    assert!(
        !addr.is_null() && addr as usize != usize::MAX,
        "page pool mmap failed (base={base:#x} pages={pages} fixed={fixed})"
    );
    let mapped = addr as usize;
    assert!(
        mapped.is_multiple_of(PAGE),
        "mmap must return page-aligned base"
    );
    state.base = mapped;
    state.pages = pages;
    state.next = 0;
    state.fail_after = usize::MAX;
}

/// 让第 `n` 次及之后的 `alloc()` 失败（测 mid-map 分配失败回滚）。
pub(crate) fn set_fail_after(n: usize) {
    STATE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .fail_after = n;
}

/// `PageAlloc` 兼容签名：返回下一个零页的"物理地址"（即真实地址，identity）。
pub(crate) fn alloc() -> Result<usize, ()> {
    let mut state = STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    if state.next >= state.fail_after {
        return Err(());
    }
    if state.next >= state.pages {
        return Err(());
    }
    let page = state.base + state.next * PAGE;
    state.next += 1;
    Ok(page)
}

/// Sv32 用：把页池固定到 2 GiB 低地址（22 位 PPN 不截断）。仅 Linux。
#[cfg(target_os = "linux")]
pub(crate) fn init_low() {
    init(0x8000_0000, 64, true);
}
