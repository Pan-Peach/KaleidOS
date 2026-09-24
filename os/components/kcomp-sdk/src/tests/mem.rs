//! `crate::mem` 的 host 锚定：包装层只解码 `0 / -errno` 并传递 view，不添加策略
//! （size / align 校验、零初始化、无账本都在 Core 侧；这里用替身验证映射形状）。

use crate::abi::{KCORE_MEMORY_VIEW_LOCAL_VA, MemoryView};
use crate::errno::Errno;
use crate::test_support;

/// 成功：view 原样交付（kind / base / len 都来自 Core 回复）；失败：负码解码成
/// `Errno`（不返回半成品 view）。
#[test]
fn mem_alloc_returns_view_and_maps_errno() {
    let _g = test_support::lock();
    test_support::reset_script();

    test_support::script_mem_acquire(0, 0x2000, 4096);
    let view = crate::mem::mem_alloc(16, 8).expect("stub acquire succeeds");
    assert_eq!(view.kind, KCORE_MEMORY_VIEW_LOCAL_VA);
    assert_eq!((view.base, view.len), (0x2000, 4096));

    test_support::script_mem_acquire(-12, 0, 0);
    assert_eq!(crate::mem::mem_alloc(16, 8), Err(Errno::ENOMEM));
}

/// release 把 Core 的状态解码成 `Result`（成功 = `Ok(())`，失败 = `Err(Errno)`）。
#[test]
fn mem_release_maps_status() {
    let _g = test_support::lock();
    test_support::reset_script();

    let view = MemoryView {
        kind: KCORE_MEMORY_VIEW_LOCAL_VA,
        reserved: 0,
        base: 0x2000,
        len: 4096,
    };
    assert_eq!(crate::mem::mem_release(view), Ok(()));

    test_support::script_mem_release(Errno::EINVAL.code());
    assert_eq!(crate::mem::mem_release(view), Err(Errno::EINVAL));
}
