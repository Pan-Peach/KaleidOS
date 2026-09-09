//! 组件 → Core 稳定 API（EXPORT_SYMBOL 教学版）。
//!
//! 白名单原则（与 oracle 设计一致）：
//! - 导出即契约：表内条目锁定（名字 + C ABI 签名），永不做破坏性修改；
//! - 未导出的内核函数组件"看不见"——内核内部随便重构，组件零影响；
//! - 只导出"已提交真相的查询"与最小输出通道，不导出任何 authority 授予点
//!   （alloc/free、注册表变更器、TaskTable::create、调度钩子——等未来类型化授权 API）。
//! - 组件侧声明方式：`unsafe extern "C" { #[link_name = "kcore_..."] ... }`，
//!   loader 重定位时按未 mangled 字节名精确匹配；找不到 → UnresolvedSymbol 加载失败。

use crate::component::registry;
use crate::machine;
use crate::memory;
use crate::task;
use arch::{Console, ConsoleImpl};

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

static EXPORTS: [Export; 8] = [
    Export {
        name: b"kcore_console_write_byte",
        address: ExportAddress(kcore_console_write_byte as *const ()),
    },
    Export {
        name: b"kcore_log_line",
        address: ExportAddress(kcore_log_line as *const ()),
    },
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
        assert!(resolve(b"kcore_console_write_byte").is_some());
        assert!(resolve(b"kcore_log_line").is_some());
        assert!(resolve(b"kcore_machine_boot_hart").is_some());
        assert!(resolve(b"kcore_machine_cpu_count").is_some());
        assert!(resolve(b"kcore_machine_has_hart").is_some());
        assert!(resolve(b"kcore_free_page_count").is_some());
        assert!(resolve(b"kcore_task_count").is_some());
        assert!(resolve(b"kcore_component_count").is_some());
    }

    #[test]
    fn rejects_unknown_names() {
        assert_eq!(resolve(b"kcore_frame_alloc"), None);
        assert_eq!(resolve(b"kcore_"), None);
        assert_eq!(resolve(b""), None);
    }

    #[test]
    fn names_are_exact_not_prefix() {
        assert!(resolve(b"kcore_log_line2").is_none(), "禁止前缀匹配");
        assert!(resolve(b"x?kcore_log_line").is_none(), "禁止后缀匹配");
    }
}
