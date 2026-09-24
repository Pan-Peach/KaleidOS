//! mem —— 组件面向的 **raw backing 便利分配器**（`docs/architecture/memory-and-heap.md` §2）。
//!
//! [`kcore_memory_acquire`](crate::abi::kcore_memory_acquire) /
//! [`kcore_memory_release`](crate::abi::kcore_memory_release) 是 Core 的域视图 ABI；
//! 本模块是它的人体工学包装：返回 / 接受 [`MemoryView`]，把裸 `i32`
//! （`0` / `-errno`）解码成 [`Result`]。
//!
//! **这不是 per-instance 堆**（那是 [`crate::heap`] + [`crate::alloc`] 的职责）：
//! 这里是"向 Core 取一段 backing"的薄封装；普通 `malloc` / `free` 不该走它
//! （heap 只在 free list 放不下时经它的 backing 回调走这里）。
//!
//! 契约（与 Core 逐字一致，本层不添加策略）：
//!
//! - `min_len > 0`、`min_align` 为非零 2 的幂；成功时 `view.len >= min_len`，
//!   首次交付**零初始化**；失败 = `-Errno` 且不写 out；
//! - 释放凭**同一个** view 原样交回；**无账本**——`view` 自身（`base` / `len`）
//!   就是身份，Core 不记 owner、不发 id。
//!
//! C 侧镜像：`include/kcomp_mem.h` + `c/kcomp_mem.c`（同一语义，`0` / `-errno`）。

use crate::abi::{MemoryView, kcore_memory_acquire, kcore_memory_release};
use crate::errno::{Errno, Result};

/// 取一段 backing，返回**本执行域访问窗口**（`view.base` / `view.len`）。
///
/// 成功 = `Ok(view)`（`view.len >= min_len`，首次交付零初始化）；
/// 失败 = `Err(Errno)`（`EFAULT` out 为空 / `EINVAL` size/align 非法 /
/// `EOVERFLOW` `min_len` 超出本域指针宽 / `ENOMEM` 物理内存耗尽）。
pub fn mem_alloc(min_len: u64, min_align: u64) -> Result<MemoryView> {
    let mut view = MemoryView {
        kind: 0,
        reserved: 0,
        base: 0,
        len: 0,
    };
    // SAFETY: view 是本帧可写输出位置；Core 契约保证失败时不写、成功时写完整。
    let status = unsafe { kcore_memory_acquire(min_len, min_align, &mut view) };
    if status < 0 {
        return Err(Errno::from_code(status));
    }
    Ok(view)
}

/// 交回一个 [`mem_alloc`] 交付的 view：把 backing 归还分配器。
///
/// KernelNative 是**受信操作**（不校验归属、无账本），`(base, len)` 必须与
/// acquire 交付的 view 完全一致。成功 = `Ok(())`；失败 = `Err(Errno)`。
pub fn mem_release(view: MemoryView) -> Result<()> {
    // SAFETY: view 是本地值，`&view` 在调用期间有效；Core 只读它。
    let status = unsafe { kcore_memory_release(&view) };
    if status < 0 {
        return Err(Errno::from_code(status));
    }
    Ok(())
}
