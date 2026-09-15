//! 第 3 组（resource authority）：MMIO / IRQ / DMA 的通用链 + Core 已定义的拒绝路径。
//!
//! # 边界：这里没有平台事实
//!
//! 只按 compatible 枚举（名字来自机器自己的 discovery）、认领 Core 给的
//! `DeviceId`、经 Core 句柄访问，断言 Core 自己报告的 errno / 派生结果。
//! QEMU virt 的**平台白盒**事实（PLIC 线号、S-mode context 公式、enable bit
//! 控制器布局/读回）不在这里 —— 那是 ArchTest `external-irq`
//! （`os/boot/riscv/src/selftest.rs`）的职责，它直接驱动 PLIC + UART 验证
//! “硬件真的被写”。CoreTest 只验证组件能走完 Core 的授权链。
//!
//! # DMA 授权模型（刻意如此，别把“没建模”读成疏漏）
//!
//! 1. **不建模“设备是不是 DMA master”**：FDT 没有可靠来源（真实 QEMU virt DTB
//!    只在 `/soc/pci@30000000` 上标 `dma-coherent`，virtio-mmio / uart 节点
//!    什么都不带），组件也无法可靠自报。
//! 2. `kcore_dma_alloc` 的授权证明 = caller **已持有该设备的 `MmioHandle`**
//!    （Core 从 handle 推导设备身份，不接受组件自报设备号）。
//! 3. 这是**协作式信任**（cooperative trust），本阶段刻意接受：KernelNative
//!    组件与 Core 同特权、按设计同级信任，本就不承诺恶意隔离。
//! 4. **未决问题**：组件目前可以自己 claim 中断控制器（PLIC）等设备 ——
//!    “认领一台设备 = 拿到它的全部语义（含控制其他设备的中断线）”这个能力
//!    （capability）问题还没有人回答；记录在此以免被当成遗漏。

use kcomp_sdk::DmaDirection;
use kcomp_sdk::abi::{
    kcore_device_nth, kcore_dma_alloc, kcore_dma_lease, kcore_dma_release, kcore_irq_ack,
    kcore_irq_claim, kcore_irq_enable, kcore_irq_poll, kcore_irq_register,
    kcore_irq_register_polled, kcore_irq_release, kcore_mmio_claim, kcore_mmio_lease,
    kcore_mmio_read_u32, kcore_mmio_release, kcore_mmio_write_u32,
};

use super::report::Checks;
use super::trace;

/// Core `errno.rs` 稳定数值的镜像（本组只断言，不解释）。
const ENOENT: i32 = -2;
const EBUSY: i32 = -16;
const EINVAL: i32 = -22;
const ESTALE: i32 = -116;

/// VirtIO MMIO transport 的 MagicValue（VirtIO 规范，设备身份而非平台事实）。
const VIRTIO_MMIO_MAGIC: u32 = 0x7472_6976;
/// Goldfish RTC 的 IRQ_ENABLED 寄存器（4 字节 RW，读回精确回显）。
/// 不用 virtio-mmio Status 做写回读：不挂后端设备时 QEMU 对 transport 的寄存器
/// 写一律忽略（读也只回 magic/version/vendor），而 RTC 一直有后端；reset 后
/// `irq_pending == 0`，写 1 无副作用。
const RTC_IRQ_ENABLED: u32 = 0x10;
/// virtio-mmio transport 寄存器窗口大小（VirtIO 规范：4 KiB）。
const VIRTIO_MMIO_WINDOW: usize = 0x1000;

/// 组件侧 IRQ 处理函数（本用例只验证注册/投递链路，不做设备 ack）。
extern "C" fn irq_handler(_ctx: *mut ()) {}

/// 本组结果：`trace` 组按这些 **Core 返回值** 做身份锚定（见 trace.rs）。
pub struct Outcome {
    /// 本组第一个 claim 之前取的 trace 游标。
    pub cursor: u64,
    /// 三个 authority 的 raw handle（= claim/alloc 返回值；匹配 grant/revoke 事件）。
    pub mmio: u64,
    pub irq: u64,
    pub dma: u64,
    /// 三个获取操作是否全部成功（失败时 trace 断言必须失败，不靠 handle 巧合）。
    pub grants_ok: bool,
    /// 三个显式释放是否全部成功。
    pub revokes_ok: bool,
}

pub fn group(checks: &mut Checks) -> Outcome {
    checks.group("resource authority");
    let cursor = trace::cursor();

    // --- MMIO root：纯枚举 → 认领**确切设备** → 经句柄读回设备身份 ---
    // QEMU 的 virtio-mmio transport 按 compatible 被机器发现；组件全程不持有地址。
    let mut virtio_device = 0u32;
    let enumerated = unsafe {
        kcore_device_nth(
            b"virtio,mmio".as_ptr(),
            b"virtio,mmio".len(),
            0,
            &mut virtio_device,
        ) == 0
    };
    let mut mmio = 0u64;
    let mmio_claimed = enumerated && unsafe { kcore_mmio_claim(virtio_device, &mut mmio) } == 0;
    let mut magic = 0u32;
    let magic_ok = mmio_claimed && unsafe { kcore_mmio_read_u32(mmio, 0, &mut magic) } == 0;
    checks.check(10, "mmio-magic", magic_ok && magic == VIRTIO_MMIO_MAGIC);

    // 拒绝路径：独占锚在**设备**上 —— 同一 DeviceId 重复认领 → -EBUSY。
    let mut duplicate = 0u64;
    checks.check(
        18,
        "mmio-double-claim",
        mmio_claimed && unsafe { kcore_mmio_claim(virtio_device, &mut duplicate) } == EBUSY,
    );

    // --- IRQ：从 UART 的 MMIO root 派生**同台设备**的中断线（不按 compatible 另配）---
    let mut uart_device = 0u32;
    let uart_enumerated = unsafe {
        kcore_device_nth(b"ns16550a".as_ptr(), b"ns16550a".len(), 0, &mut uart_device) == 0
    };
    let mut uart = 0u64;
    let uart_claimed = uart_enumerated && unsafe { kcore_mmio_claim(uart_device, &mut uart) } == 0;
    let mut irq = 0u64;
    let irq_claimed = uart_claimed && unsafe { kcore_irq_claim(uart, &mut irq) } == 0;

    // 拒绝路径：使能前必须先注册投递 —— 未注册就 enable → -EINVAL
    // （Core 在碰控制器之前拒绝）。
    checks.check(
        19,
        "irq-enable-order",
        irq_claimed && unsafe { kcore_irq_enable(irq) } == EINVAL,
    );

    let irq_registered =
        irq_claimed && unsafe { kcore_irq_register(irq, irq_handler, core::ptr::null_mut()) } == 0;
    let irq_enabled = irq_registered && unsafe { kcore_irq_enable(irq) } == 0;
    // 只断言 Core 报告的整链成功；“控制器寄存器真的被写”由 ArchTest 覆盖。
    checks.check(
        11,
        "irq-line-enable",
        irq_claimed && irq_registered && irq_enabled,
    );

    // 拒绝路径：同一条线重复认领（即使还是同一个 root）→ -EBUSY。
    let mut irq_again = 0u64;
    checks.check(
        20,
        "irq-double-claim",
        irq_claimed && unsafe { kcore_irq_claim(uart, &mut irq_again) } == EBUSY,
    );

    // 拒绝路径：root 生命周期 —— 仍有 live IRQ 子 authority 时释放 MMIO root
    // → -EBUSY（优雅拆机顺序：先释放子项再放 root）。
    checks.check(
        21,
        "mmio-release-busy",
        irq_claimed && unsafe { kcore_mmio_release(uart) } == EBUSY,
    );

    // --- MMIO lease：一次性派生 (ptr, len)，只读一个 u32（MagicValue）作证 ---
    let mut lease_ptr = 0usize;
    let mut lease_len = 0usize;
    let leased =
        mmio_claimed && unsafe { kcore_mmio_lease(mmio, &mut lease_ptr, &mut lease_len) } == 0;
    let lease_magic =
        leased && unsafe { core::ptr::read_volatile(lease_ptr as *const u32) } == VIRTIO_MMIO_MAGIC;
    checks.check(
        12,
        "mmio-lease",
        leased && lease_len == VIRTIO_MMIO_WINDOW && lease_magic,
    );

    // --- MMIO 写回读：goldfish RTC 的 IRQ_ENABLED（0x10，4 字节 RW，读回精确
    //     回显）。写 1 → 读回应为 1 → 还原原值并释放。RTC 一直有后端，写读回
    //     在 test-qemu（不挂设备的 virtio transport）之外同样成立。 ---
    let mut rtc_device = 0u32;
    let rtc_enumerated = unsafe {
        kcore_device_nth(
            b"google,goldfish-rtc".as_ptr(),
            b"google,goldfish-rtc".len(),
            0,
            &mut rtc_device,
        ) == 0
    };
    let mut rtc = 0u64;
    let rtc_claimed = rtc_enumerated && unsafe { kcore_mmio_claim(rtc_device, &mut rtc) } == 0;
    let mut rtc_state = 0u32;
    let rtc_read =
        rtc_claimed && unsafe { kcore_mmio_read_u32(rtc, RTC_IRQ_ENABLED, &mut rtc_state) } == 0;
    let rtc_wrote = rtc_read && unsafe { kcore_mmio_write_u32(rtc, RTC_IRQ_ENABLED, 1) } == 0;
    let mut rtc_echo = 0u32;
    let rtc_echo_read =
        rtc_wrote && unsafe { kcore_mmio_read_u32(rtc, RTC_IRQ_ENABLED, &mut rtc_echo) } == 0;
    let rtc_restored =
        rtc_echo_read && unsafe { kcore_mmio_write_u32(rtc, RTC_IRQ_ENABLED, rtc_state) } == 0;
    let rtc_released = rtc_restored && unsafe { kcore_mmio_release(rtc) } == 0;
    checks.check(
        13,
        "mmio-write-readback",
        rtc_restored && rtc_released && rtc_echo == 1,
    );

    // --- MMIO release：显式撤销 authority 后同一 handle 立即失效（过期 → -ESTALE）---
    let mmio_released = mmio_claimed && unsafe { kcore_mmio_release(mmio) } == 0;
    let mut stale_value = 0u32;
    let stale_read = unsafe { kcore_mmio_read_u32(mmio, 0, &mut stale_value) };
    checks.check(14, "mmio-release", mmio_released && stale_read == ESTALE);

    // 拒绝路径：死 root 不能派生新 authority —— IRQ / DMA 请求都 → -ESTALE
    // （root 释放后 slot generation 前进，旧 raw handle 一律过期）。
    let mut irq_from_dead = 0u64;
    let irq_denied = unsafe { kcore_irq_claim(mmio, &mut irq_from_dead) } == ESTALE;
    let mut dma_from_dead = 0u64;
    let dma_denied = unsafe {
        kcore_dma_alloc(
            mmio,
            4096,
            DmaDirection::ToDevice.as_i32(),
            &mut dma_from_dead,
        )
    } == ESTALE;
    checks.check(
        22,
        "stale-root-derive",
        mmio_released && irq_denied && dma_denied,
    );

    // --- IRQ polled：把已 enable 的线切成轮询投递 → poll 计数（run 中无 UART
    //     中断 = 0）→ ack 闭环。不 enable 该线、不碰 UART IER/THR。 ---
    let polled = irq_enabled && unsafe { kcore_irq_register_polled(irq) } == 0;
    let mut poll_count = 0u64;
    let poll_read = polled && unsafe { kcore_irq_poll(irq, &mut poll_count) } == 0;
    let poll_acked = poll_read && unsafe { kcore_irq_ack(irq) } == 0;
    checks.check(15, "irq-polled", poll_read && poll_count == 0 && poll_acked);

    // --- IRQ release：真正撤销该线（撤销 slot + 关断控制器线）后同一 handle
    //     立即失效（过期 → -ESTALE）。 ---
    let irq_released = poll_acked && unsafe { kcore_irq_release(irq) } == 0;
    let mut released_count = 0u64;
    let stale_poll = unsafe { kcore_irq_poll(irq, &mut released_count) } == ESTALE;
    checks.check(17, "irq-release", irq_released && stale_poll);

    // --- DMA：用仍持有的 UART MmioHandle 推导设备身份（授权模型见模块文档）---
    // alloc → lease backing → 写读回 0xDEADBEEF → release → 后续 lease 过期。
    let mut dma = 0u64;
    let dma_allocated = uart_claimed
        && unsafe { kcore_dma_alloc(uart, 8192, DmaDirection::Bidirectional.as_i32(), &mut dma) }
            == 0;
    let mut dma_ptr = 0usize;
    let mut dma_len = 0usize;
    let mut dma_device_addr = 0u64;
    let dma_leased = dma_allocated
        && unsafe { kcore_dma_lease(dma, &mut dma_ptr, &mut dma_len, &mut dma_device_addr) } == 0;
    let dma_roundtrip = dma_leased
        && dma_ptr != 0
        && dma_len >= 8192
        && dma_device_addr != 0
        && unsafe {
            core::ptr::write_volatile(dma_ptr as *mut u32, 0xDEAD_BEEF);
            core::ptr::read_volatile(dma_ptr as *const u32) == 0xDEAD_BEEF
        };
    let dma_released = dma_roundtrip && unsafe { kcore_dma_release(dma) } == 0;
    let mut stale_ptr = 0usize;
    let mut stale_len = 0usize;
    let mut stale_addr = 0u64;
    let dma_stale =
        unsafe { kcore_dma_lease(dma, &mut stale_ptr, &mut stale_len, &mut stale_addr) } == ESTALE;
    checks.check(16, "dma-ring", dma_roundtrip && dma_released && dma_stale);

    // 拒绝路径：不存在的 ordinal 是 discovery 的唯一终止信号 → -ENOENT。
    let mut missing = 0u32;
    let miss = unsafe {
        kcore_device_nth(b"ns16550a".as_ptr(), b"ns16550a".len(), 1, &mut missing) == ENOENT
    };
    checks.check(23, "device-ordinal-miss", miss);

    Outcome {
        cursor,
        mmio,
        irq,
        dma,
        grants_ok: mmio_claimed && irq_claimed && dma_allocated,
        revokes_ok: mmio_released && irq_released && dma_released,
    }
}
