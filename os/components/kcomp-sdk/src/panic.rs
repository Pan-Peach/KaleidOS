//! 组件 panic adapter（组件私有；见 crate 文档 # panic adapter）。
//!
//! 组件以链接后的 ET_REL 加载，镜像里带自己的 `#[panic_handler]`——组件 `panic!`
//! 时进入的是这里，而不是 boot 镜像的 panic handler。adapter 只做两件事：经
//! [`abi::kcore_log_line`] 打印一行诊断，然后调 [`abi::kcore_panic_escape`]
//! 把控制权交还 Core（活动 containment 边界内它**永不返回**）。若没有活动边界
//! （返回 `-EPERM`），说明这次 panic 不在任何组件边界内，只能停在原地自旋（安全失败）。

use crate::abi;
use crate::logging::{LOG_LINE_BYTES, LineBuffer};

/// 组件 panic handler：打印诊断后协作式逃逸回 Core。
#[panic_handler]
fn component_panic(info: &core::panic::PanicInfo<'_>) -> ! {
    let mut bytes = [0u8; LOG_LINE_BYTES];
    let mut writer = LineBuffer {
        bytes: &mut bytes,
        length: 0,
    };
    // `kcore_log_line` 会加 `[kcomp] ` 前缀，这里只写消息体。
    let _ = core::fmt::write(&mut writer, format_args!("panic"));
    if let Some(location) = info.location() {
        let _ = core::fmt::write(
            &mut writer,
            format_args!(" at {}:{}", location.file(), location.line()),
        );
    }
    let _ = core::fmt::write(&mut writer, format_args!(": {}", info.message()));
    let length = writer.length;
    // SAFETY: 同 `log`：只读本帧缓冲；诊断必须活到 escape 之前。
    unsafe {
        abi::kcore_log_line(bytes.as_ptr(), length);
    }
    // 活动边界内永不返回；无边界时返回 -EPERM → 停在原地（安全失败）。
    unsafe {
        abi::kcore_panic_escape();
    }
    loop {
        core::hint::spin_loop();
    }
}
