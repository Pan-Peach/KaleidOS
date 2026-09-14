//! DMA 方向：ABI 编码的类型化镜像。
//!
//! Core 与 SDK 各自持有一份声明（组件不能依赖 `os/core`——那会把 Core 的 Rust
//! 类型与代码带进 `.kcomp`，违反"不建 shared runtime / 组件只经 `kcore_*` 交互"）；
//! 两侧各有锚定测试把值钉死在 0/1/2，防止漂移。

/// DMA 传输方向。**这是 Component ABI 的一部分**：编码 `0/1/2`，与 Core
/// `handle/dma.rs::DmaDirection::as_i32` 及 `kcore_dma_alloc` 的 `direction`
/// 参数一致（见 `docs/driver-model.md` §6.2）。
///
/// 设备库的枚举（如 `virtio_drivers::BufferDirection`）到本枚举的映射写在**驱动
/// 组件**里（纯类型匹配，不出现数字）。
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DmaDirection {
    /// 内存 → 设备。
    ToDevice = 0,
    /// 设备 → 内存。
    FromDevice = 1,
    /// 双向。
    Bidirectional = 2,
}

impl DmaDirection {
    /// ABI 编码（`#[repr(i32)]`，恒等于判别值）。
    pub const fn as_i32(self) -> i32 {
        self as i32
    }
}
