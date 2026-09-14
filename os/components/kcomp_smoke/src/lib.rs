//! kcomp_smoke —— SDK 参考组件（step 2 C）。
//!
//! 通过 Component SDK 调用 `kcore_*` 白名单（不再自己写 extern / console helper），
//! 验证链接后的 `.kcomp` 重定位链路（UNDEF 只解析白名单符号），并保持与迁移前
//! 相同的可观测行为：输出 `[smoke] hex=<n>\n!`（`n` = 组件数 + 空闲页数）。

#![no_std]

use kcomp_sdk::abi;

fn hex_digit(d: u32) -> u8 {
    if d < 10 {
        b'0' + d as u8
    } else {
        b'a' + (d - 10) as u8
    }
}

/// 16 进制输出：移位 + 掩码，无除法（避免 div_by_zero panic 分支）、无数组（避免 拷贝/清零 libcall）。
fn write_hex(v: u32) {
    let mut started = false;
    let mut shift = u32::BITS as usize - 4;
    loop {
        let d = (v >> shift) & 0xF;
        if started || d != 0 || shift == 0 {
            kcomp_sdk::console_write_byte(hex_digit(d));
            started = true;
        }
        if shift == 0 {
            break;
        }
        shift -= 4;
    }
}

fn do_smoke() -> u32 {
    // SAFETY: 两个只读查询导出无参数、无所有权语义。
    let components = unsafe { abi::kcore_component_count() };
    let free_pages = unsafe { abi::kcore_free_page_count() };
    components.wrapping_add(free_pages)
}

kcomp_sdk::kcomp_init!({
    let n = do_smoke();
    for &c in "[smoke] hex=".as_bytes() {
        kcomp_sdk::console_write_byte(c);
    }
    write_hex(n);
    kcomp_sdk::console_write_byte(b'\n');
    kcomp_sdk::console_write_byte(b'!');
    0
});

/// 组件退出入口（Linux `module_exit` 类比）——本演示组件是 no-op：定义它是为了
/// 让 loader 的 optional-exit seam 有真实符号可解析，并断言 `.kcomp` 流程
/// （partial link / GC）不会丢掉它。
///
/// **Core 本轮只解析、永不调用**（见 `os/core/src/component/loader.rs` 的
/// `LoadedComponent::exit` 与 `TODO(component-exit)`）。
#[unsafe(no_mangle)]
pub extern "C" fn kcomp_exit() -> i32 {
    0
}
