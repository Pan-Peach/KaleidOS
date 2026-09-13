//! kcomp-sdk —— 组件 SDK / CRT（step 2）。
//!
//! 这个 crate 解决三件互相独立的事，全部**随 `.kcomp` 私有携带**（不建 shared
//! Rust runtime，见 docs/component-model.md §2.2）：
//!
//! 1. [`abi`]：`kcore_*` 导出白名单的**单一来源**（组件不再各自复制 extern 块）；
//! 2. 入口 / 日志 / panic adapter：`kcomp_init!`、[`log`]/`klog!`、[`panic_handler!`]；
//! 3. 可选的 alloc adapter（feature `alloc`）：`GlobalAlloc` → Core 共享堆。
//!
//! # panic adapter（本 crate 存在的关键理由）
//!
//! 组件以链接后的 ET_REL 加载，镜像里带自己的 `#[panic_handler]`——组件 `panic!`
//! 时进入的是这里，而不是 boot 镜像的 panic handler。adapter 只做两件事：
//! 经 [`abi::kcore_log_line`] 打印一行诊断，然后调 [`abi::kcore_panic_escape`]
//! 把控制权交还 Core（活动 containment 边界内它**永不返回**，见
//! `component/containment.rs`）。若没有活动边界（返回 `-EPERM`），说明这次 panic
//! 不在任何组件边界内，只能停在原地自旋（安全失败）。
//!
//! # alloc adapter
//!
//! `#[global_allocator]` 不是"每个组件自带堆"：它只是把 Rust `GlobalAlloc`
//! 契约接到 **Core 共享堆**（`kcore_heap_alloc/dealloc`）。默认关闭，组件按需
//! 通过 `kcomp-sdk = { path = "...", features = ["alloc"] }` 开启。

#![no_std]

// host 测试用（`cargo test`）；裸机目标不编入。
#[cfg(test)]
extern crate std;

/// `kcore_*` 导出 ABI（EXPORT_SYMBOL 教学版）。
///
/// 声明即契约：名字必须与 Core `component/export.rs` 的白名单逐字节一致，签名
/// 错误 = UB（loader 只按名字精确解析，不校验签名）。这里保持**全量**声明，
/// 让各组件只共用这一份；新增 Core 导出时同步加在这里。
pub mod abi {
    /// IRQ 投递回调：`ctx` 原样回传，Core 不解引用。
    pub type IrqHandler = extern "C" fn(ctx: *mut ());

    // 安全说明：以下符号由 Core 保证实现；调用方必须满足各自契约（指针有效性、
    // out 参数可写、handle 归宿等），故调用点均为 `unsafe`。
    unsafe extern "C" {
        // -- Runtime / shared heap（Core 共享堆，非 per-component 堆）--
        #[link_name = "kcore_heap_alloc"]
        pub fn kcore_heap_alloc(size: usize, align: usize) -> *mut u8;
        #[link_name = "kcore_heap_dealloc"]
        pub fn kcore_heap_dealloc(ptr: *mut u8, size: usize, align: usize) -> i32;

        // -- Logging / diagnostics --
        #[link_name = "kcore_console_write_byte"]
        pub fn kcore_console_write_byte(byte: u8);
        #[link_name = "kcore_log_line"]
        pub fn kcore_log_line(ptr: *const u8, len: usize) -> i32;

        // -- Machine query --
        #[link_name = "kcore_machine_boot_hart"]
        pub fn kcore_machine_boot_hart() -> u32;
        #[link_name = "kcore_machine_cpu_count"]
        pub fn kcore_machine_cpu_count() -> u32;
        #[link_name = "kcore_machine_has_hart"]
        pub fn kcore_machine_has_hart(hart_id: u32) -> i32;

        // -- System query --
        #[link_name = "kcore_free_page_count"]
        pub fn kcore_free_page_count() -> u32;
        #[link_name = "kcore_task_count"]
        pub fn kcore_task_count() -> u32;
        #[link_name = "kcore_component_count"]
        pub fn kcore_component_count() -> u32;

        // -- Component lifecycle --
        #[link_name = "kcore_component_load"]
        pub fn kcore_component_load(name: *const u8, len: usize) -> i32;
        #[link_name = "kcore_interface_publish"]
        pub fn kcore_interface_publish(
            name: *const u8,
            len: usize,
            kind: u32,
            version: u32,
            context: *mut (),
        ) -> i32;
        #[link_name = "kcore_interface_available"]
        pub fn kcore_interface_available(
            name: *const u8,
            len: usize,
            kind: u32,
            version: u32,
        ) -> i32;

        // -- Task control --
        #[link_name = "kcore_task_create"]
        pub fn kcore_task_create(entry: usize) -> i32;
        #[link_name = "kcore_task_start"]
        pub fn kcore_task_start(id: u32) -> i32;
        #[link_name = "kcore_task_yield"]
        pub fn kcore_task_yield() -> i32;
        #[link_name = "kcore_task_exit"]
        pub fn kcore_task_exit() -> i32;
        #[link_name = "kcore_task_state"]
        pub fn kcore_task_state(id: u32) -> i32;

        // -- Panic containment --
        #[link_name = "kcore_panic_escape"]
        pub fn kcore_panic_escape() -> i32;

        // -- Scheduler --
        #[link_name = "kcore_sched_run"]
        pub fn kcore_sched_run() -> i32;

        // -- Resource authority: MMIO --
        #[link_name = "kcore_mmio_claim"]
        pub fn kcore_mmio_claim(name: *const u8, len: usize, out_handle: *mut u64) -> i32;
        #[link_name = "kcore_mmio_read_u32"]
        pub fn kcore_mmio_read_u32(handle: u64, offset: u32, out_value: *mut u32) -> i32;
        #[link_name = "kcore_mmio_write_u32"]
        pub fn kcore_mmio_write_u32(handle: u64, offset: u32, value: u32) -> i32;
        #[link_name = "kcore_mmio_release"]
        pub fn kcore_mmio_release(handle: u64) -> i32;
        #[link_name = "kcore_mmio_lease"]
        pub fn kcore_mmio_lease(handle: u64, out_ptr: *mut usize, out_len: *mut usize) -> i32;

        // -- Resource authority: DMA --
        #[link_name = "kcore_dma_alloc"]
        pub fn kcore_dma_alloc(
            mmio_handle: u64,
            size: usize,
            direction: i32,
            out_handle: *mut u64,
        ) -> i32;
        #[link_name = "kcore_dma_lease"]
        pub fn kcore_dma_lease(
            handle: u64,
            out_ptr: *mut usize,
            out_len: *mut usize,
            out_device_addr: *mut u64,
        ) -> i32;
        #[link_name = "kcore_dma_release"]
        pub fn kcore_dma_release(handle: u64) -> i32;

        // -- Resource authority: IRQ --
        #[link_name = "kcore_irq_claim"]
        pub fn kcore_irq_claim(name: *const u8, len: usize, out_handle: *mut u64) -> i32;
        #[link_name = "kcore_irq_register"]
        pub fn kcore_irq_register(handle: u64, handler: IrqHandler, ctx: *mut ()) -> i32;
        #[link_name = "kcore_irq_enable"]
        pub fn kcore_irq_enable(handle: u64) -> i32;
        #[link_name = "kcore_irq_register_polled"]
        pub fn kcore_irq_register_polled(handle: u64) -> i32;
        #[link_name = "kcore_irq_poll"]
        pub fn kcore_irq_poll(handle: u64, out_count: *mut u64) -> i32;
        #[link_name = "kcore_irq_ack"]
        pub fn kcore_irq_ack(handle: u64) -> i32;
    }
}

// ---------------------------------------------------------------------------
// DMA 方向：ABI 编码的类型化镜像
// ---------------------------------------------------------------------------

/// DMA 传输方向。**这是 Component ABI 的一部分**：编码 `0/1/2`，与 Core
/// `handle/dma.rs::DmaDirection::as_i32` 及 `kcore_dma_alloc` 的 `direction`
/// 参数一致（见 `docs/driver-model.md` §6.2）。
///
/// Core 与 SDK 各自持有一份声明（组件不能依赖 `os/core`——那会把 Core 的 Rust
/// 类型与代码带进 `.kcomp`，违反"不建 shared runtime / 组件只经 `kcore_*` 交互"）；
/// 两侧各有锚定测试把值钉死在 0/1/2，防止漂移。设备库的枚举（如
/// `virtio_drivers::BufferDirection`）到本枚举的映射写在**驱动组件**里（纯类型
/// 匹配，不出现数字）。
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

// ---------------------------------------------------------------------------
// 日志：固定缓冲 + kcore_log_line（无 alloc、无锁；行尾由 Core 追加）
// ---------------------------------------------------------------------------

/// 单行诊断上限（超长截断；panic 路径不需要无限长消息）。
const LOG_LINE_BYTES: usize = 256;

/// 把 `core::fmt` 写进固定栈缓冲。
struct LineBuffer<'a> {
    bytes: &'a mut [u8],
    length: usize,
}

impl core::fmt::Write for LineBuffer<'_> {
    fn write_str(&mut self, value: &str) -> core::fmt::Result {
        let source = value.as_bytes();
        let remaining = self.bytes.len() - self.length;
        let take = source.len().min(remaining);
        self.bytes[self.length..self.length + take].copy_from_slice(&source[..take]);
        self.length += take;
        Ok(())
    }
}

/// 输出一行：`core::fmt` 格式化到栈缓冲，再一次 `kcore_log_line`（Core 加
/// `[kcomp] ` 前缀与换行）。不分配、不持锁，可在 panic 路径使用。
pub fn log(args: core::fmt::Arguments<'_>) {
    let mut bytes = [0u8; LOG_LINE_BYTES];
    let mut writer = LineBuffer {
        bytes: &mut bytes,
        length: 0,
    };
    let _ = core::fmt::write(&mut writer, args);
    let length = writer.length;
    // SAFETY: (ptr, len) 指向本帧内已初始化的字节；Core 只读该区间。
    unsafe {
        abi::kcore_log_line(bytes.as_ptr(), length);
    }
}

/// 组件日志宏：`klog!("state={}", value)` → [`log`]。
#[macro_export]
macro_rules! klog {
    ($($arg:tt)*) => {
        $crate::log(::core::format_args!($($arg)*))
    };
}

/// 直写一字节（无缓冲；需要与日志行交错时用）。
pub fn console_write_byte(byte: u8) {
    // SAFETY: Core 保证该导出线程/中断安全地写 arch Console backend。
    unsafe {
        abi::kcore_console_write_byte(byte);
    }
}

// ---------------------------------------------------------------------------
// 组件入口约定
// ---------------------------------------------------------------------------

/// 定义加载入口 `kcomp_init`（Linux module_init 风格）。
///
/// 用法：`kcomp_sdk::kcomp_init!({ ...; 0 })`。块的值即返回码：`0` = 成功，
/// 非 0 = 失败位图（Core 据此标记 Failed）。与手写
/// `#[unsafe(no_mangle)] pub extern "C" fn kcomp_init() -> i32` 完全等价。
#[macro_export]
macro_rules! kcomp_init {
    ($($body:tt)*) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn kcomp_init() -> i32 {
            $($body)*
        }
    };
}

// ---------------------------------------------------------------------------
// panic adapter（组件私有；见 crate 文档）
// ---------------------------------------------------------------------------

/// 组件 panic handler：打印诊断后协作式逃逸回 Core。
///
/// 只在裸机目标生成——host 构建（clippy / workspace test）下 std 自带 panic
/// handler，重复定义会冲突。
#[cfg(target_os = "none")]
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

// ---------------------------------------------------------------------------
// alloc adapter（feature `alloc`）：GlobalAlloc → Core 共享堆
// ---------------------------------------------------------------------------

#[cfg(all(target_os = "none", feature = "alloc"))]
mod global_alloc {
    use crate::abi;
    use core::alloc::{GlobalAlloc, Layout};

    /// 薄 adapter：不拥有内存，只把 Rust `GlobalAlloc` 契约转成 Core 共享堆 ABI。
    struct CoreHeap;

    unsafe impl GlobalAlloc for CoreHeap {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            // SAFETY: layout 由 GlobalAlloc 契约保证合法（size>0、align 为 2 的幂）；
            // Core 侧再次校验，失败返回 null。
            unsafe { abi::kcore_heap_alloc(layout.size(), layout.align()) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: ptr 来自同一 layout 的一次成功 alloc（GlobalAlloc 契约）。
            unsafe {
                let _ = abi::kcore_heap_dealloc(ptr, layout.size(), layout.align());
            }
        }
    }

    #[global_allocator]
    static CORE_HEAP: CoreHeap = CoreHeap;
}

#[cfg(test)]
mod tests {
    //! host 锚定测试：钉住 ABI 编码（`docs/driver-model.md` §6.2）。
    //! Core 侧有对应测试 `component::export::tests::dma_direction_encoding_is_stable`。

    #[test]
    fn dma_direction_encoding_is_stable() {
        use super::DmaDirection::{Bidirectional, FromDevice, ToDevice};
        assert_eq!(ToDevice.as_i32(), 0);
        assert_eq!(FromDevice.as_i32(), 1);
        assert_eq!(Bidirectional.as_i32(), 2);
    }
}
