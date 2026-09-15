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

// TODO(component-exit): 退出收尾（停 DMA / mask IRQ / 释放 authority）——Core 只解析、从不调用，当前显式 no-op。
kcomp_sdk::kcomp_exit!(0);
