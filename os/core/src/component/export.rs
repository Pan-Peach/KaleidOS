//! 组件 → Core 稳定 API（EXPORT_SYMBOL 教学版，v1）。
//!
//! # 白名单原则（与 oracle 设计一致）
//! - 导出即契约：表内条目锁定（名字 + C ABI 签名），永不做破坏性修改；
//! - 未导出的内核函数组件"看不见"——内核内部随便重构，组件零影响；
//! - 未导出符号 → loader `UnresolvedSymbol`，整次加载失败（exact-name resolution）；
//! - 组件侧声明方式：`unsafe extern "C" { #[link_name = "kcore_..."] ... }`，
//!   loader 重定位时按未 mangled 字节名精确匹配。
//!
//! # ABI 分类（v1，稳定）
//!
//! | 类别 | 符号 | 说明 |
//! |---|---|---|
//! | Runtime / shared heap | `kcore_heap_alloc` `kcore_heap_dealloc` | KernelNative 组件与 Core 共享堆的分配/释放（契约 = Rust `GlobalAlloc`）。**不是**物理区域/帧分配、**不是**地址空间变更——这些 authority 敏感操作永不裸导出 |
//! | Logging / diagnostics | `kcore_console_write_byte` `kcore_log_line` | 输出通道（传输在 arch `Console` backend） |
//! | Machine query | `kcore_machine_boot_hart` `kcore_machine_cpu_count` `kcore_machine_has_hart` | 已提交机器真相的只读查询 |
//! | System query | `kcore_free_page_count` `kcore_task_count` `kcore_component_count` | 已提交 Core 真相的只读查询 |
//!
//! # 明确不导出（authority 授予点 / Core truth 变更点）
//!
//! - 物理内存：`memory::alloc_region` / `free_region` / `vm_page_alloc`——物理帧是
//!   Core 内部机制（canonical），组件要内存走 `kcore_heap_alloc`（共享堆）。
//! - 地址空间：`KernelAddressSpace::map/unmap/activate`——mutation 必须过 Core
//!   验证与 commit，且需要 `AddressSpaceHandle`（未来类型化授权 API）。
//! - 任务表：`TaskTable::create` / `set_task_state` / context switch——绕过 Core
//!   truth 的 mutation 一律不导出；调度走 propose → validate → commit。
//! - 注册表：`registry::declare/start/unload`——组件生命周期由 Core 掌控。
//! - Trace 事件：组件未来只能提交"组件自定义事件"，`TaskSwitch/Grant/Revoke/
//!   CoreRejected` 等 Core authoritative event 由 Core 自己产生（TODO：trace
//!   环形缓冲落地后加 `kcore_trace_component_event`，sequence 由 Core 分配）。
//!
//! # 共享堆 ABI 的所有权/生命周期语义
//!
//! `kcore_heap_alloc/dealloc` 是 KernelNative 组件共享 Core 堆的入口（AGENTS.md：
//! Core 与组件共享一个 Core heap，无 per-component 记账）。契约与 Rust
//! `GlobalAlloc` 完全一致：dealloc 的 `(ptr, size, align)` 必须与一次成功的 alloc
//! 严格匹配，违反 = UB（与 C `malloc/free` 错配同类）。组件失败后的泄漏在 phase 1
//! 可接受（不承诺共享堆字节回收，见 roadmap §8）；完整回收留给未来 ExecutionDomain。

use crate::component::registry;
use crate::machine;
use crate::memory;
use crate::task;
use arch::{Console, ConsoleImpl};
use core::alloc::GlobalAlloc;

/// 单个导出条目：公开字节名 + 内核侧函数地址。
/// 地址以裸函数指针存静态——rustc 生成普通数据重定位，最终链接器填入真实地址，
/// 无需 build script / 运行时注册。
struct Export {
    name: &'static [u8],
    address: ExportAddress,
}

/// 包装裸函数指针：`Sync` 安全（条目不可变，指向已链接的可执行文本）。
#[repr(transparent)]
struct ExportAddress(*const ());

unsafe impl Sync for ExportAddress {}

// ---------------------------------------------------------------------------
// Category 1：Runtime / shared heap（KernelNative 组件共享堆）
// ---------------------------------------------------------------------------

/// 共享堆分配。契约 = Rust `GlobalAlloc::alloc`：`align` 必须为 2 的幂，
/// `size > 0`；失败返回 null。所有权归调用方组件；Core 不做 per-component 记账。
///
/// # Safety
/// 返回指针的释放必须通过 `kcore_heap_dealloc`（携带相同 size/align）。
extern "C" fn kcore_heap_alloc(size: usize, align: usize) -> *mut u8 {
    // C ABI 语义：size==0 或非法 align 一律失败返回 null。
    // （Rust `Layout` 允许空 layout，但 C 风格调用方可能传 0——显式拒绝。）
    if size == 0 {
        return core::ptr::null_mut();
    }
    let Ok(layout) = core::alloc::Layout::from_size_align(size, align) else {
        return core::ptr::null_mut();
    };
    // SAFETY: layout 已由 from_size_align 验证；KernelAllocator 是共享堆的
    // GlobalAlloc 实现（host test 下经 test_support::ensure_init 就绪）。
    unsafe { memory::KernelAllocator.alloc(layout) }
}

/// 共享堆释放。契约 = Rust `GlobalAlloc::dealloc`（见模块文档的语义说明）。
///
/// # Safety
/// `ptr` 必须来自一次成功的 `kcore_heap_alloc`，且 `(size, align)` 必须与那次
/// 调用完全一致。违反 = UB。
extern "C" fn kcore_heap_dealloc(ptr: *mut u8, size: usize, align: usize) -> i32 {
    if ptr.is_null() {
        return -1;
    }
    let Ok(layout) = core::alloc::Layout::from_size_align(size, align) else {
        return -1;
    };
    // SAFETY: 由调用方保证 ptr/layout 匹配一次成功 alloc（C ABI 契约）。
    unsafe {
        memory::KernelAllocator.dealloc(ptr, layout);
    }
    0
}

// ---------------------------------------------------------------------------
// Category 2：Logging / diagnostics
// ---------------------------------------------------------------------------

extern "C" fn kcore_console_write_byte(byte: u8) {
    ConsoleImpl::write_byte(byte);
}

extern "C" fn kcore_log_line(ptr: *const u8, len: usize) -> i32 {
    if (ptr.is_null() && len != 0) || len > isize::MAX as usize {
        return -1;
    }
    const PREFIX: &[u8] = b"[kcomp] ";
    for &b in PREFIX {
        ConsoleImpl::write_byte(b);
    }
    if len > 0 {
        let bytes = unsafe { core::slice::from_raw_parts(ptr, len) };
        for &b in bytes {
            ConsoleImpl::write_byte(b);
        }
    }
    ConsoleImpl::write_byte(b'\n');
    0
}

// ---------------------------------------------------------------------------
// Category 3：Machine query（已提交机器真相的只读查询）
// ---------------------------------------------------------------------------

extern "C" fn kcore_machine_boot_hart() -> usize {
    machine::committed().map_or(0, |m| m.boot_hart)
}

extern "C" fn kcore_machine_cpu_count() -> usize {
    machine::committed().map_or(0, |m| m.cpu_count)
}

extern "C" fn kcore_machine_has_hart(hart_id: usize) -> i32 {
    let Some(machine) = machine::committed() else {
        return 0;
    };
    machine.cpu_info[..machine.cpu_count.min(machine.cpu_info.len())]
        .iter()
        .any(|cpu| cpu.hart_id.raw() == hart_id) as i32
}

// ---------------------------------------------------------------------------
// Category 4：System query（已提交 Core 真相的只读查询）
// ---------------------------------------------------------------------------

extern "C" fn kcore_free_page_count() -> usize {
    memory::free_block_counts()
        .iter()
        .enumerate()
        .skip(memory::HEAP_MIN_ORDER)
        .map(|(order, &blocks)| blocks * (1usize << (order - memory::HEAP_MIN_ORDER)))
        .sum()
}

extern "C" fn kcore_task_count() -> usize {
    task::get_task_table().lock().len()
}

extern "C" fn kcore_component_count() -> usize {
    registry::get_registry().lock().len()
}

// ---------------------------------------------------------------------------
// 导出表（v1 白名单；添加符号 = 破坏性 ABI 变更，必须同步 bump 文档）
// ---------------------------------------------------------------------------

static EXPORTS: [Export; 10] = [
    // Category 1：Runtime / shared heap
    Export {
        name: b"kcore_heap_alloc",
        address: ExportAddress(kcore_heap_alloc as *const ()),
    },
    Export {
        name: b"kcore_heap_dealloc",
        address: ExportAddress(kcore_heap_dealloc as *const ()),
    },
    // Category 2：Logging / diagnostics
    Export {
        name: b"kcore_console_write_byte",
        address: ExportAddress(kcore_console_write_byte as *const ()),
    },
    Export {
        name: b"kcore_log_line",
        address: ExportAddress(kcore_log_line as *const ()),
    },
    // Category 3：Machine query
    Export {
        name: b"kcore_machine_boot_hart",
        address: ExportAddress(kcore_machine_boot_hart as *const ()),
    },
    Export {
        name: b"kcore_machine_cpu_count",
        address: ExportAddress(kcore_machine_cpu_count as *const ()),
    },
    Export {
        name: b"kcore_machine_has_hart",
        address: ExportAddress(kcore_machine_has_hart as *const ()),
    },
    // Category 4：System query
    Export {
        name: b"kcore_free_page_count",
        address: ExportAddress(kcore_free_page_count as *const ()),
    },
    Export {
        name: b"kcore_task_count",
        address: ExportAddress(kcore_task_count as *const ()),
    },
    Export {
        name: b"kcore_component_count",
        address: ExportAddress(kcore_component_count as *const ()),
    },
];

/// 按未 mangled 字节名精确查找导出地址（线性扫：条目少，不值得排序/哈希）。
/// 返回的内核地址由 loader 作为 ELF 重定位的 `S` 使用。
pub fn resolve(name: &[u8]) -> Option<usize> {
    EXPORTS
        .iter()
        .find(|e| e.name == name)
        .map(|e| e.address.0 as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_all_entries() {
        use alloc::string::String;
        for name in [
            &b"kcore_heap_alloc"[..],
            &b"kcore_heap_dealloc"[..],
            &b"kcore_console_write_byte"[..],
            &b"kcore_log_line"[..],
            &b"kcore_machine_boot_hart"[..],
            &b"kcore_machine_cpu_count"[..],
            &b"kcore_machine_has_hart"[..],
            &b"kcore_free_page_count"[..],
            &b"kcore_task_count"[..],
            &b"kcore_component_count"[..],
        ] {
            assert!(resolve(name).is_some(), "{}", String::from_utf8_lossy(name));
        }
    }

    #[test]
    fn rejects_unknown_names() {
        assert_eq!(resolve(b"kcore_frame_alloc"), None);
        assert_eq!(resolve(b"kcore_alloc_region"), None);
        assert_eq!(resolve(b"kcore_address_space_map"), None);
        assert_eq!(resolve(b"kcore_task_create"), None);
        assert_eq!(resolve(b"kcore_"), None);
        assert_eq!(resolve(b""), None);
    }

    #[test]
    fn names_are_exact_not_prefix() {
        assert!(resolve(b"kcore_log_line2").is_none(), "禁止前缀匹配");
        assert!(resolve(b"x?kcore_log_line").is_none(), "禁止后缀匹配");
    }

    #[test]
    fn heap_alloc_dealloc_roundtrip() {
        let _g = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let alloc = resolve(b"kcore_heap_alloc").unwrap();
        let dealloc = resolve(b"kcore_heap_dealloc").unwrap();
        let alloc: extern "C" fn(usize, usize) -> *mut u8 = unsafe { core::mem::transmute(alloc) };
        let dealloc: extern "C" fn(*mut u8, usize, usize) -> i32 =
            unsafe { core::mem::transmute(dealloc) };

        let p = alloc(32, 8);
        assert!(!p.is_null(), "共享堆必须能分配");
        // 写读往返，验证可写
        unsafe {
            core::ptr::write_volatile(p as *mut u64, 0xDEAD_BEEF);
            assert_eq!(core::ptr::read_volatile(p as *mut u64), 0xDEAD_BEEF);
        }
        assert_eq!(dealloc(p, 32, 8), 0);
    }

    #[test]
    fn heap_alloc_invalid_layout_returns_null() {
        let _g = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let alloc = resolve(b"kcore_heap_alloc").unwrap();
        let alloc: extern "C" fn(usize, usize) -> *mut u8 = unsafe { core::mem::transmute(alloc) };
        // size=0 与非法 align（非 2 的幂）必须返回 null，不得 panic/UB。
        assert!(alloc(0, 8).is_null());
        assert!(alloc(16, 3).is_null());
    }
}
