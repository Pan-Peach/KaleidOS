//! 第 3 组（device / IRQ / DMA）：discover → claim → 直接访问 → release 的完整链，
//! 以及 Core 已定义的拒绝路径。
//!
//! # 边界：这里没有平台事实
//!
//! 只按 compatible 枚举（名字来自机器自己的 discovery）、认领 Core 给的
//! `DeviceId`、**直接 volatile 访问** Core 返回的 MMIO 指针，断言 Core 自己报告的
//! errno。QEMU virt 的**平台白盒**事实（PLIC 线号、enable bit 布局/读回）不在这里
//! ——那是 ArchTest `external-irq` 的职责。
//!
//! # mechanism-first 模型（刻意如此）
//!
//! - KernelNative claim 后直接拿到寄存器裸指针，**不存在** per-access Core 鉴权；
//!   越界/对齐由 driver 自己负责（这里不再有 `mmio-read-bounds` 检查）。
//! - IRQ 锚在 `DeviceId`：register/enable/disable/release；没有 poll/ack。
//! - DMA allocation 与 mapping 分离：`alloc → buffer`，`map(device_id) → device addr`。

use kcomp_sdk::DmaDirection;
use kcomp_sdk::abi::{
    kcore_device_claim, kcore_device_nth, kcore_device_release, kcore_dma_alloc, kcore_dma_free,
    kcore_dma_map, kcore_dma_unmap, kcore_irq_disable, kcore_irq_enable, kcore_irq_register,
    kcore_irq_release,
};
use kcomp_sdk::errno::Errno;

use super::report::Checks;
use super::trace;

/// VirtIO MMIO transport 的 MagicValue（VirtIO 规范，设备身份而非平台事实）。
const VIRTIO_MMIO_MAGIC: u32 = 0x7472_6976;
/// Goldfish RTC 的 IRQ_ENABLED 寄存器（4 字节 RW，读回精确回显）。
const RTC_IRQ_ENABLED: usize = 0x10;
/// virtio-mmio transport 寄存器窗口大小（VirtIO 规范：4 KiB）。
const VIRTIO_MMIO_WINDOW: usize = 0x1000;

/// 组件侧 IRQ 处理函数（本用例只验证注册/投递链路，不做设备 ack）。
extern "C" fn irq_handler(_ctx: *mut ()) {}

/// 本组结果：`trace` 组按这些 **Core 返回值** 做身份锚定（见 trace.rs）。
pub struct Outcome {
    /// 本组第一个 claim 之前取的 trace 游标。
    pub cursor: u64,
    /// 三者的资源 id（与 trace `ResourceGrant.c` 精确匹配）：
    /// device = DeviceId（= device_index），irq = DeviceId，dma = mapping id。
    pub device: u64,
    pub irq: u64,
    pub dma: u64,
    /// 三个获取操作是否全部成功。
    pub grants_ok: bool,
    /// 三个显式释放是否全部成功。
    pub revokes_ok: bool,
}

/// 直接读 32-bit MMIO 寄存器（driver 自己的 volatile 访问，Core 不参与）。
unsafe fn read_u32(base: *mut u8, offset: usize) -> u32 {
    unsafe { core::ptr::read_volatile((base as usize + offset) as *const u32) }
}

/// 直接写 32-bit MMIO 寄存器。
unsafe fn write_u32(base: *mut u8, offset: usize, value: u32) {
    unsafe { core::ptr::write_volatile((base as usize + offset) as *mut u32, value) };
}

pub fn group(checks: &mut Checks) -> Outcome {
    checks.group("device / irq / dma");
    let cursor = trace::cursor();

    // --- VirtIO device：枚举 → claim → 直接 volatile 读 MagicValue ---
    let mut virtio_device = 0u32;
    let enumerated = unsafe {
        kcore_device_nth(
            b"virtio,mmio".as_ptr(),
            b"virtio,mmio".len(),
            0,
            &mut virtio_device,
        ) == 0
    };
    let (mut mmio, mut mmio_len) = (core::ptr::null_mut(), 0usize);
    let claimed =
        enumerated && unsafe { kcore_device_claim(virtio_device, &mut mmio, &mut mmio_len) } == 0;
    let magic_ok = claimed && unsafe { read_u32(mmio, 0) } == VIRTIO_MMIO_MAGIC;
    checks.check(10, "device-claim-magic", magic_ok);
    checks.check(
        11,
        "device-window-len",
        claimed && mmio_len == VIRTIO_MMIO_WINDOW,
    );

    // 拒绝路径：独占锚在**设备**上 —— 同一 DeviceId 重复认领 → -EBUSY。
    let (mut dup, mut dup_len) = (core::ptr::null_mut(), 0usize);
    checks.check(
        12,
        "device-double-claim",
        claimed
            && unsafe { kcore_device_claim(virtio_device, &mut dup, &mut dup_len) }
                == Errno::EBUSY.code(),
    );

    // --- UART：claim 后用于 IRQ / DMA ---
    let mut uart_device = 0u32;
    let uart_enumerated = unsafe {
        kcore_device_nth(b"ns16550a".as_ptr(), b"ns16550a".len(), 0, &mut uart_device) == 0
    };
    let (mut uart, mut uart_len) = (core::ptr::null_mut(), 0usize);
    let uart_claimed = uart_enumerated
        && unsafe { kcore_device_claim(uart_device, &mut uart, &mut uart_len) } == 0;

    // 拒绝路径：使能前必须先注册 handler —— 未注册就 enable → -EINVAL。
    checks.check(
        13,
        "irq-enable-order",
        uart_claimed && unsafe { kcore_irq_enable(uart_device) } == Errno::EINVAL.code(),
    );

    let irq_registered = uart_claimed
        && unsafe { kcore_irq_register(uart_device, irq_handler, core::ptr::null_mut()) } == 0;
    let irq_enabled = irq_registered && unsafe { kcore_irq_enable(uart_device) } == 0;
    let irq_disabled = irq_enabled && unsafe { kcore_irq_disable(uart_device) } == 0;
    checks.check(
        14,
        "irq-line-enable",
        irq_registered && irq_enabled && irq_disabled,
    );

    // 拒绝路径：仍有 live IRQ route 时释放 device → -EBUSY（拆机顺序）。
    checks.check(
        15,
        "device-release-busy",
        irq_registered && unsafe { kcore_device_release(uart_device) } == Errno::EBUSY.code(),
    );

    // --- IRQ release：撤销 route 后重复释放 → -EINVAL（该设备已无 route）。 ---
    let irq_released = irq_registered && unsafe { kcore_irq_release(uart_device) } == 0;
    let irq_double = unsafe { kcore_irq_release(uart_device) } == Errno::EINVAL.code();
    checks.check(16, "irq-release", irq_released && irq_double);

    // --- Device release：释放后同一设备可被再次认领（无 quarantine）。 ---
    let uart_released = uart_claimed && unsafe { kcore_device_release(uart_device) } == 0;
    let (mut uart2, mut uart2_len) = (core::ptr::null_mut(), 0usize);
    let uart_reclaimed = uart_released
        && unsafe { kcore_device_claim(uart_device, &mut uart2, &mut uart2_len) } == 0;
    checks.check(17, "device-release", uart_released && uart_reclaimed);

    // --- MMIO 直接写回读：goldfish RTC 的 IRQ_ENABLED（0x10，4 字节 RW）。 ---
    let mut rtc_device = 0u32;
    let rtc_enumerated = unsafe {
        kcore_device_nth(
            b"google,goldfish-rtc".as_ptr(),
            b"google,goldfish-rtc".len(),
            0,
            &mut rtc_device,
        ) == 0
    };
    let (mut rtc, mut rtc_len) = (core::ptr::null_mut(), 0usize);
    let rtc_claimed =
        rtc_enumerated && unsafe { kcore_device_claim(rtc_device, &mut rtc, &mut rtc_len) } == 0;
    let rtc_state = rtc_claimed && {
        let value = unsafe { read_u32(rtc, RTC_IRQ_ENABLED) };
        unsafe { write_u32(rtc, RTC_IRQ_ENABLED, 1) };
        let echo = unsafe { read_u32(rtc, RTC_IRQ_ENABLED) };
        unsafe { write_u32(rtc, RTC_IRQ_ENABLED, value) };
        echo == 1
    };
    let rtc_released = rtc_claimed && unsafe { kcore_device_release(rtc_device) } == 0;
    checks.check(18, "device-write-readback", rtc_state && rtc_released);

    // --- DMA：allocation 与 mapping 分离；alloc → map → 写读回 → unmap → free。 ---
    let (mut dma_ptr, mut dma_len) = (core::ptr::null_mut(), 0usize);
    let dma_allocated =
        uart_reclaimed && unsafe { kcore_dma_alloc(8192, &mut dma_ptr, &mut dma_len) } == 0;
    let (mut dma_device_addr, mut dma_mapping) = (0u64, 0u64);
    let dma_mapped = dma_allocated
        && unsafe {
            kcore_dma_map(
                uart_device,
                dma_ptr,
                dma_len,
                DmaDirection::Bidirectional.as_i32(),
                &mut dma_device_addr,
                &mut dma_mapping,
            )
        } == 0;
    let dma_roundtrip = dma_mapped
        && !dma_ptr.is_null()
        && dma_device_addr != 0
        && unsafe {
            core::ptr::write_volatile(dma_ptr as *mut u32, 0xDEAD_BEEF);
            core::ptr::read_volatile(dma_ptr as *const u32) == 0xDEAD_BEEF
        };
    let dma_unmapped = dma_roundtrip && unsafe { kcore_dma_unmap(dma_mapping) } == 0;
    let dma_freed = dma_unmapped && unsafe { kcore_dma_free(dma_ptr) } == 0;
    checks.check(19, "dma-ring", dma_roundtrip && dma_unmapped && dma_freed);

    // 拒绝路径：DMA 尺寸 0 非法 → -EINVAL。
    let (mut zero_ptr, mut zero_len) = (core::ptr::null_mut(), 0usize);
    checks.check(
        20,
        "dma-invalid-size",
        unsafe { kcore_dma_alloc(0, &mut zero_ptr, &mut zero_len) } == Errno::EINVAL.code(),
    );

    // 拒绝路径：不存在的 ordinal 是 discovery 的唯一终止信号 → -ENOENT。
    let mut missing = 0u32;
    let miss = unsafe {
        kcore_device_nth(b"ns16550a".as_ptr(), b"ns16550a".len(), 1, &mut missing)
            == Errno::ENOENT.code()
    };
    checks.check(21, "device-ordinal-miss", miss);

    // 拒绝路径：释放未认领的设备 → -ENODEV（已释放的 RTC）。
    checks.check(
        22,
        "device-release-unclaimed",
        unsafe { kcore_device_release(rtc_device) } == Errno::ENODEV.code(),
    );

    // 清理：释放 virtio claim（其 trace revoke 也在本组窗口内）。
    let virtio_released = claimed && unsafe { kcore_device_release(virtio_device) } == 0;

    Outcome {
        cursor,
        device: u64::from(virtio_device),
        irq: u64::from(uart_device),
        dma: dma_mapping,
        grants_ok: claimed && irq_registered && dma_mapped,
        revokes_ok: virtio_released && irq_released && dma_unmapped,
    }
}
