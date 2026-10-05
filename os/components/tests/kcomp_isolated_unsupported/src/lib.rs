//! kcomp_isolated_unsupported —— Isolated import **拒绝**的 ArchTest 夹具。
//!
//! import `kcore_heap_alloc`：它只属于 KernelNative 共享堆后端。
//! 装载必须在声明 / 放段 / 登记之前显式拒绝
//! （`IsolatedImportUnsupported`），绝不静默解析成裸 Core 地址。

#![no_std]

use kcomp_sdk as _;

kcomp_sdk::kcomp_instance_create!(|_args, _out_state| {
    // 本函数永远不会在 Isolated 域里被执行（装载前被拒）；保留调用只为让
    // `kcore_heap_alloc` 成为真实的 UNDEF import。
    // SAFETY: 只构造调用形状；Core 在装载前就拒绝该 import。
    let ptr = unsafe { kcomp_sdk::abi::kcore_heap_alloc(32, 8) };
    if ptr.is_null() { -12 } else { 0 }
});

kcomp_sdk::kcomp_instance_destroy!(|_state| { 0 });
