//! kcomp_smoke —— 第二个 .kcomp 组件（升级版）：
//! 通过导出表（白名单）调用内核函数，验证重定位链路（UNDEF 符号表解析 + 组件内符号）。

#![no_std]

// 白名单 API 声明：未 mangled 精确名 + C ABI（与内核 `export.rs` 一一对应）。
// 签名错 = UB（声明即契约），loader 只按名字解析，不校验签名。
unsafe extern "C" {
    #[link_name = "kcore_console_write_byte"]
    fn console_write_byte(byte: u8);
    #[link_name = "kcore_component_count"]
    fn component_count() -> usize;
    #[link_name = "kcore_free_page_count"]
    fn free_page_count() -> usize;
}

fn hex_digit(d: usize) -> u8 {
    if d < 10 {
        b'0' + d as u8
    } else {
        b'a' + (d - 10) as u8
    }
}

/// 16 进制输出：移位 + 掩码，无除法（避免 div_by_zero panic 分支）、无数组（避免 拷贝/清零 libcall）。
fn write_hex(v: usize) {
    let mut started = false;
    let mut shift = usize::BITS as usize - 4;
    loop {
        let d = (v >> shift) & 0xF;
        if started || d != 0 || shift == 0 {
            unsafe {
                console_write_byte(hex_digit(d));
            }
            started = true;
        }
        if shift == 0 {
            break;
        }
        shift -= 4;
    }
}

fn do_smoke() -> usize {
    unsafe { component_count().wrapping_add(free_page_count()) }
}

/// 组件入口：加载器在放段 + 重定位之后调用。
/// 返回约定（Linux insmod 风格）：0 = 加载成功；非 0 = 加载失败。
#[unsafe(no_mangle)]
pub extern "C" fn kcomp_init() -> i32 {
    let n = do_smoke();
    unsafe {
        for &c in "[smoke] hex=".as_bytes() {
            console_write_byte(c);
        }
        write_hex(n);
        console_write_byte(b'\n');
        console_write_byte(b'!');
    }
    0
}
