//! CoreTest 测试组件（第一个 .kcomp）：核内自检 Core 的真实接口。
//!
//! - `kcomp_init`：loader 放段 + 重定位后调用；返回 0 = 全部通过，
//!   非 0 = 失败位图（`load` 命令会据此报告 FAILED）
//! - 自检项（核内，QEMU `load core_test` 时真实执行）：
//!   `.data` 段搬运 / 机器真相 / 内存分配器 / 组件注册表 /
//!   **C4 执行链**（组件加载 → 接口发布/绑定 → 任务创建 → RR 调度 →
//!   上下文切换 → yield/exit → 状态验证）/
//!   **C6 MMIO 链**（claim 设备 → 经 handle 读真实寄存器 → lease 派生裸指针 →
//!   write/read 回写 → release 后 handle 即失效）/
//!   **C6 IRQ 链**（claim 设备中断线 → 注册处理函数 → 使能 → PLIC enable bit 读回 →
//!   polled 轮询投递 → poll/ack 闭环）/
//!   **C6 DMA 链**（用 MMIO handle 推导设备 → alloc → lease 写读回 → release → stale）
//! - 只走导出白名单（`kcore_*`），无 god-mode；不直接触碰 TaskTable /
//!   Registry / Sv39 / CpuContext —— 那是 Core 的真相
//! - host 测试只覆盖纯逻辑；真实执行在核内

#![no_std]

// 组件私有 panic adapter（kcomp-sdk）：只提供裸机 #[panic_handler] + 链接期
// Rust support，符号未被引用时被 --gc-sections 丢弃。
use kcomp_sdk as _;

#[cfg(test)]
extern crate std;

/// 静态数据，.data 段。放段/重定位正确性锚点：值原样保持 = 段被正确搬运。
static MAGIC: u32 = 0xC0FFEE;

/// 纯逻辑（host-testable）：.data 段的值在放段后必须原样可读。
fn data_ok() -> bool {
    MAGIC == 0xC0FFEE
}

/// Validate the machine facts supplied by the Core exports.  The actual hart
/// membership lookup stays in Core; this pure predicate remains host-testable.
fn machine_ok(cpu_count: u32, boot_hart_present: bool) -> bool {
    cpu_count >= 1 && boot_hart_present
}

/// 核内自检：仅在非 test 编译生成（组件 .kcomp / 真机）。
/// host `cargo test` 不引用 `kcore_*`，避免未定义符号链接失败；
/// 组件镜像里的 UNDEF 符号由 loader 按导出白名单重定位解析。
#[cfg(not(test))]
mod runtime {
    use super::{data_ok, machine_ok};

    // 白名单 API（与 kernel `export.rs` 一一对应；C ABI 声明即契约）。
    unsafe extern "C" {
        #[link_name = "kcore_console_write_byte"]
        fn console_write_byte(byte: u8);
        #[link_name = "kcore_machine_boot_hart"]
        fn machine_boot_hart() -> u32;
        #[link_name = "kcore_machine_cpu_count"]
        fn machine_cpu_count() -> u32;
        #[link_name = "kcore_machine_has_hart"]
        fn machine_has_hart(hart_id: u32) -> i32;
        #[link_name = "kcore_free_page_count"]
        fn free_page_count() -> u32;
        #[link_name = "kcore_component_count"]
        fn component_count() -> u32;

        // C4：组件生命周期 / 接口 / 任务 / 调度
        #[link_name = "kcore_component_load"]
        fn component_load(name: *const u8, len: usize) -> i32;
        #[link_name = "kcore_interface_available"]
        fn interface_available(name: *const u8, len: usize, kind: u32, abi: u64) -> i32;
        #[link_name = "kcore_task_create"]
        fn task_create(entry: usize) -> i32;
        #[link_name = "kcore_task_start"]
        fn task_start(id: u32) -> i32;
        #[link_name = "kcore_task_yield"]
        fn task_yield() -> i32;
        #[link_name = "kcore_task_exit"]
        fn task_exit() -> i32;
        #[link_name = "kcore_task_state"]
        fn task_state(id: u32) -> i32;
        #[link_name = "kcore_sched_run"]
        fn sched_run() -> i32;

        // C6：资源 authority（status + out 形态，0 / -Errno）
        #[link_name = "kcore_device_nth"]
        fn device_nth(
            compatible: *const u8,
            len: usize,
            ordinal: u32,
            out_device_id: *mut u32,
        ) -> i32;
        #[link_name = "kcore_mmio_claim"]
        fn mmio_claim(device_id: u32, out_handle: *mut u64) -> i32;
        #[link_name = "kcore_mmio_read_u32"]
        fn mmio_read_u32(handle: u64, offset: u32, out_value: *mut u32) -> i32;
        #[link_name = "kcore_mmio_write_u32"]
        fn mmio_write_u32(handle: u64, offset: u32, value: u32) -> i32;
        #[link_name = "kcore_mmio_release"]
        fn mmio_release(handle: u64) -> i32;
        #[link_name = "kcore_mmio_lease"]
        fn mmio_lease(handle: u64, out_ptr: *mut usize, out_len: *mut usize) -> i32;

        // C6：IRQ authority（status + out / status 形态，0 / -Errno）
        #[link_name = "kcore_irq_claim"]
        fn irq_claim(mmio_handle: u64, out_handle: *mut u64) -> i32;
        #[link_name = "kcore_irq_register"]
        fn irq_register(handle: u64, handler: extern "C" fn(*mut ()), ctx: *mut ()) -> i32;
        #[link_name = "kcore_irq_enable"]
        fn irq_enable(handle: u64) -> i32;
        #[link_name = "kcore_irq_register_polled"]
        fn irq_register_polled(handle: u64) -> i32;
        #[link_name = "kcore_irq_poll"]
        fn irq_poll(handle: u64, out_count: *mut u64) -> i32;
        #[link_name = "kcore_irq_ack"]
        fn irq_ack(handle: u64) -> i32;
        #[link_name = "kcore_irq_release"]
        fn irq_release(handle: u64) -> i32;

        // C6：DMA authority（alloc → lease → release，0 / -Errno）
        #[link_name = "kcore_dma_alloc"]
        fn dma_alloc(mmio_handle: u64, size: usize, direction: i32, out_handle: *mut u64) -> i32;
        #[link_name = "kcore_dma_lease"]
        fn dma_lease(
            handle: u64,
            out_ptr: *mut usize,
            out_len: *mut usize,
            out_device_addr: *mut u64,
        ) -> i32;
        #[link_name = "kcore_dma_release"]
        fn dma_release(handle: u64) -> i32;
    }

    /// 组件侧 IRQ 处理函数（本用例只验证注册/投递链路，不做设备 ack）。
    extern "C" fn irq_handler(_ctx: *mut ()) {}

    // ABI 编码常量（与 Core export.rs 一致）。
    const KIND_POLICY: u32 = 2;
    const STATE_EXITED: i32 = 4;
    /// SchedulerPolicy 的 exact ABI fingerprint（SDK 统一定义；与 Core
    /// `sched::SCHEDULER_POLICY_ABI` 一致）。
    const SCHEDULER_POLICY_ABI: u64 = kcomp_sdk::binding::SCHEDULER_POLICY_ABI.raw();

    // ---- 输出样式（ANSI SGR；终端解释颜色，日志里是可剥离的控制字节）----
    //
    // 约束（tests/qemu/runner.py）：runner 用**连续子串**匹配
    // `[core-test] all: PASS` 和 `load core_test: OK`，且把 "FAIL" 当作
    // fatal marker。所以颜色码只能包在标记之外，绝不能插进标记内部。
    mod style {
        pub const RESET: &[u8] = b"\x1b[0m";
        pub const BOLD_RED: &[u8] = b"\x1b[1;31m";
        pub const BOLD_GREEN: &[u8] = b"\x1b[1;32m";
        pub const BOLD_CYAN: &[u8] = b"\x1b[1;36m";
    }

    fn puts(s: &str) {
        for &b in s.as_bytes() {
            unsafe {
                console_write_byte(b);
            }
        }
    }

    fn puts_bytes(bytes: &[u8]) {
        for &b in bytes {
            unsafe {
                console_write_byte(b);
            }
        }
    }

    /// 颜色包裹输出：`<color><text><reset>`。
    fn puts_colored(color: &[u8], text: &str) {
        puts_bytes(color);
        puts(text);
        puts_bytes(style::RESET);
    }

    /// 十进制输出（无 alloc 的极简实现，汇总计数用）。
    fn put_usize(mut n: usize) {
        let mut buf = [0u8; 20];
        let mut i = buf.len();
        loop {
            i -= 1;
            buf[i] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        puts_bytes(&buf[i..]);
    }

    /// 测试报告器：分组 + 计数 + 颜色（PASS 绿 / FAIL 红）。
    struct Reporter {
        passed: usize,
        total: usize,
    }

    impl Reporter {
        const fn new() -> Self {
            Self {
                passed: 0,
                total: 0,
            }
        }

        /// 组头：`[core-test] ── <title> ──`（青色加粗，整段一次着色）。
        fn group(&mut self, title: &str) {
            puts("[core-test] ");
            puts_bytes(style::BOLD_CYAN);
            puts("── ");
            puts(title);
            puts(" ──");
            puts_bytes(style::RESET);
            puts("\n");
        }

        /// 一项检查：`[core-test]   <name>: PASS|FAIL`（缩进属于组）。返回是否通过。
        fn check(&mut self, name: &str, ok: bool) -> bool {
            puts("[core-test]   ");
            puts(name);
            puts(": ");
            if ok {
                puts_colored(style::BOLD_GREEN, "PASS");
            } else {
                puts_colored(style::BOLD_RED, "FAIL");
            }
            puts("\n");
            self.total += 1;
            if ok {
                self.passed += 1;
            }
            ok
        }

        /// 汇总计数：`[core-test]   <passed>/<total> checks PASS`（全过绿，否则红）。
        fn summary(&self) -> bool {
            let all_ok = self.passed == self.total;
            puts("[core-test]   ");
            put_usize(self.passed);
            puts("/");
            put_usize(self.total);
            puts(" checks ");
            if all_ok {
                puts_colored(style::BOLD_GREEN, "PASS");
            } else {
                puts_colored(style::BOLD_RED, "FAIL");
            }
            puts("\n");
            all_ok
        }

        /// 终判行：**必须**保持 `[core-test] all: PASS` 为连续子串
        /// （runner 标记）。颜色码放在行首/行尾，标记本体不动。
        fn verdict(&self, ok: bool) {
            puts_bytes(if ok {
                style::BOLD_GREEN
            } else {
                style::BOLD_RED
            });
            puts(if ok {
                "[core-test] all: PASS"
            } else {
                "[core-test] all: FAIL"
            });
            puts_bytes(style::RESET);
            puts("\n");
        }
    }

    /// 内存分配器已初始化（存在可分配空闲页）。
    fn memory_ok() -> bool {
        let free = unsafe { free_page_count() };
        free > 0
    }

    /// 组件注册表已登记（至少包含当前组件）。
    fn component_ok() -> bool {
        let count = unsafe { component_count() };
        count >= 1
    }

    // ---- C4 执行链场景 ------------------------------------------------

    /// 任务 A/B 的迭代计数（任务体只做计数 + yield；验证由 kcomp_init 在
    /// 调度返回后读取）。KernelNative 单 CPU，无并发。
    static mut A_COUNT: usize = 0;
    static mut B_COUNT: usize = 0;

    /// 纯逻辑：任务计数在 3 轮 yield 后必须各自为 3（可 host 测试）。
    const EXPECTED_ITERS: usize = 3;

    extern "C" fn task_a() -> ! {
        for _ in 0..EXPECTED_ITERS {
            unsafe {
                A_COUNT += 1;
                task_yield();
            }
        }
        unsafe {
            task_exit();
        }
        // task_exit 永不返回本任务；防御性驻留（不可达但满足 -> !）。
        loop {
            core::hint::spin_loop();
        }
    }

    extern "C" fn task_b() -> ! {
        for _ in 0..EXPECTED_ITERS {
            unsafe {
                B_COUNT += 1;
                task_yield();
            }
        }
        unsafe {
            task_exit();
        }
        loop {
            core::hint::spin_loop();
        }
    }
    /// 组件入口（Linux module_init 约定）：0 = 全部通过；非 0 = 失败位图。
    ///
    /// 输出分组：boot basics（M0/M1 基础链）/ scheduling chain（C4 执行链）/
    /// summary（计数 + 终判）。PASS 绿、FAIL 红；组头青色。
    #[unsafe(no_mangle)]
    pub extern "C" fn kcomp_init() -> i32 {
        let mut failed = 0u32;
        let mut report = Reporter::new();

        macro_rules! check {
            ($name:expr, $ok:expr, $bit:expr) => {
                if !report.check($name, $ok) {
                    failed |= 1 << $bit;
                }
            };
        }

        report.group("boot basics");
        check!("data", data_ok(), 0);
        let cpus = unsafe { machine_cpu_count() };
        let boot = unsafe { machine_boot_hart() };
        let boot_present = unsafe { machine_has_hart(boot) } != 0;
        check!("machine", machine_ok(cpus, boot_present), 1);
        check!("memory", memory_ok(), 2);
        check!("component", component_ok(), 3);

        report.group("scheduling chain");
        // scheduler_rr 加载（组件 → Core ABI → 加载链）
        let rr_id = unsafe { component_load(b"scheduler_rr".as_ptr(), b"scheduler_rr".len()) };
        check!("scheduler-load", rr_id >= 0, 4);

        // 任务创建：requester = core_test（Core 从 call_init 上下文解析），
        // entry = task_a/task_b（本组件镜像内的函数地址）。
        let a = unsafe { task_create(task_a as *const () as usize) };
        let b = unsafe { task_create(task_b as *const () as usize) };
        check!("task-create", a >= 0 && b >= 0 && a != b, 5);

        // 调度器接口已绑定且 provider 存活（发布发生在 scheduler_rr 的 init）。
        let bound = unsafe {
            interface_available(
                b"scheduler".as_ptr(),
                b"scheduler".len(),
                KIND_POLICY,
                SCHEDULER_POLICY_ABI,
            ) == 1
        };
        check!("scheduler-bind", bound, 6);

        // 启动（Created → Runnable）并进入调度：跑完所有任务才返回。
        unsafe {
            task_start(a as u32);
            task_start(b as u32);
        }
        let ran = unsafe { sched_run() } == 0;

        // 切换验证：A/B 各跑满 3 轮（RR 交替），计数必须各自为 3。
        let counts_ok = unsafe { A_COUNT == EXPECTED_ITERS && B_COUNT == EXPECTED_ITERS };
        check!("task-switch", ran && counts_ok, 7);

        // 退出验证：两个任务都已 Exited（终态由 Core 状态机提交）。
        let exited =
            unsafe { task_state(a as u32) == STATE_EXITED && task_state(b as u32) == STATE_EXITED };
        check!("task-exit", exited, 8);

        // scheduler_rr 仍 Ready（resolve 会做 provider 存活二次校验）。
        let rr_ready = unsafe {
            interface_available(
                b"scheduler".as_ptr(),
                b"scheduler".len(),
                KIND_POLICY,
                SCHEDULER_POLICY_ABI,
            ) == 1
        };
        check!("scheduler-rr", rr_ready && rr_id >= 0, 9);

        // TODO(C5): 抢占链用例——两个"不 yield 的忙循环"任务被时钟强行切出
        //   （当前调度是协作式；timer 实现 + sched::on_timer_tick 接线后，
        //   在 scheduling chain 组追加 preempt check）。

        // C6 资源 authority 链：claim 一台 virtio-mmio transport，再经 handle
        // 读 MagicValue。QEMU 的 8 个 transport 无条件实例化（无需 -device），
        // offset 0 恒为 0x74726976；组件全程不持有地址。
        report.group("resource authority");
        // 纯枚举出第一台 virtio-mmio，再认领**确切的** DeviceId（不再"第一台匹配"）。
        let mut virtio_device = 0u32;
        let enumerated =
            unsafe { device_nth(b"virtio,mmio".as_ptr(), 11, 0, &mut virtio_device) } == 0;
        let mut mmio_handle = 0u64;
        let claimed = enumerated && unsafe { mmio_claim(virtio_device, &mut mmio_handle) } == 0;
        let mut magic = 0u32;
        let magic_ok = claimed && (unsafe { mmio_read_u32(mmio_handle, 0, &mut magic) } == 0);
        check!("mmio-magic", magic_ok && magic == 0x7472_6976, 10);

        // C6 IRQ 链：先从 UART 的 MMIO root 派生**同台设备**的中断线（不再独立匹配
        // compatible），再 Core 验证 register/enable 后真的把 PLIC 打开。用 PLIC
        // 自己的 MMIO 读回 enable bit 作证——跨过「Core 宣布成功」和「硬件真的被
        // 写」之间的空隙。QEMU virt 常量：UART = ns16550a = PLIC line 10；
        // S-mode context = hart*2+1。
        let mut uart_device = 0u32;
        let uart_enumerated =
            unsafe { device_nth(b"ns16550a".as_ptr(), 8, 0, &mut uart_device) } == 0;
        let mut uart = 0u64;
        let uart_ok = uart_enumerated && unsafe { mmio_claim(uart_device, &mut uart) } == 0;
        let mut irq = 0u64;
        let irq_claimed = uart_ok && unsafe { irq_claim(uart, &mut irq) } == 0;
        let irq_registered =
            irq_claimed && unsafe { irq_register(irq, irq_handler, core::ptr::null_mut()) } == 0;
        let irq_enabled = irq_registered && unsafe { irq_enable(irq) } == 0;

        // PLIC 用来读回 enable bit（IRQ 证据）并为 DMA 提供设备身份。
        let mut plic_device = 0u32;
        let plic_enumerated =
            unsafe { device_nth(b"riscv,plic0".as_ptr(), 11, 0, &mut plic_device) } == 0;
        let mut plic = 0u64;
        let plic_ok = plic_enumerated && unsafe { mmio_claim(plic_device, &mut plic) } == 0;

        let ctx = (unsafe { machine_boot_hart() } as usize) * 2 + 1;
        let mut enable_word = 0u32;
        let plic_written = irq_enabled
            && plic_ok
            && unsafe { mmio_read_u32(plic, (0x2000 + ctx * 0x80) as u32, &mut enable_word) } == 0;
        check!(
            "irq-line-enable",
            plic_written && (enable_word & (1 << 10)) != 0,
            11
        );

        // C6 MMIO lease：对已 claim 的 virtio transport 一次性派生 (ptr, len)，
        // 只读一个 u32（MagicValue）作证，不改设备状态。len 必须 = 映射长度。
        let mut lease_ptr = 0usize;
        let mut lease_len = 0usize;
        let leased = unsafe { mmio_lease(mmio_handle, &mut lease_ptr, &mut lease_len) } == 0;
        let lease_magic =
            leased && unsafe { core::ptr::read_volatile(lease_ptr as *const u32) } == 0x7472_6976;
        check!(
            "mmio-lease",
            leased && lease_len == 0x1000 && lease_magic,
            12
        );

        // C6 MMIO 写：PLIC priority 寄存器是 RW，source id 1 未使用，只碰它的
        // priority（offset 4 = 4*id），不 enable 该线；回读后还原为 0。
        let wrote = unsafe { mmio_write_u32(plic, 4, 3) } == 0;
        let mut priority = 0u32;
        let priority_read = wrote && (unsafe { mmio_read_u32(plic, 4, &mut priority) } == 0);
        check!("mmio-write-readback", priority_read && priority == 3, 13);
        let _ = unsafe { mmio_write_u32(plic, 4, 0) };

        // C6 MMIO release：显式撤销 authority 后同一 handle 立即失效（stale）。
        // 这是本次最后一次引用 mmio_handle。
        let released = unsafe { mmio_release(mmio_handle) } == 0;
        let mut stale_magic = 0u32;
        let stale_read = unsafe { mmio_read_u32(mmio_handle, 0, &mut stale_magic) };
        check!("mmio-release", released && stale_read != 0, 14);

        // C6 IRQ polled 链：把已 enable 的线切成轮询投递，读计数（run 中无 UART
        // 中断 = 0），再 ack 闭环。不 enable 该线、不碰 UART IER/THR。
        let polled = irq_enabled && unsafe { irq_register_polled(irq) } == 0;
        let mut poll_count = 0u64;
        let poll_read = polled && (unsafe { irq_poll(irq, &mut poll_count) } == 0);
        let poll_acked = poll_read && (unsafe { irq_ack(irq) } == 0);
        check!("irq-polled", poll_read && poll_count == 0 && poll_acked, 15);

        // C6 IRQ release：真正撤销该线 authority（revoke slot + 关断控制器线）后
        // 同一 handle 立即失效。
        let irq_released = poll_acked && (unsafe { irq_release(irq) } == 0);
        let mut released_count = 0u64;
        let stale_poll = unsafe { irq_poll(irq, &mut released_count) } != 0;
        check!("irq-release", irq_released && stale_poll, 17);

        // C6 DMA 链：用仍持有的 plic MmioHandle 推导设备身份，alloc → lease
        // backing → 写读回 0xDEADBEEF → release → 后续 lease stale（进 quarantine）。
        let mut dma = 0u64;
        let dma_ok = unsafe { dma_alloc(plic, 8192, 2, &mut dma) } == 0;
        let mut dma_ptr = 0usize;
        let mut dma_len = 0usize;
        let mut dma_devaddr = 0u64;
        let dma_leased = dma_ok
            && (unsafe { dma_lease(dma, &mut dma_ptr, &mut dma_len, &mut dma_devaddr) } == 0);
        let dma_roundtrip = dma_leased
            && dma_ptr != 0
            && dma_len >= 8192
            && dma_devaddr != 0
            && unsafe {
                core::ptr::write_volatile(dma_ptr as *mut u32, 0xDEAD_BEEF);
                core::ptr::read_volatile(dma_ptr as *const u32) == 0xDEAD_BEEF
            };
        let dma_released = dma_roundtrip && (unsafe { dma_release(dma) } == 0);
        let mut stale_ptr = 0usize;
        let mut stale_len = 0usize;
        let mut stale_addr = 0u64;
        let dma_stale =
            unsafe { dma_lease(dma, &mut stale_ptr, &mut stale_len, &mut stale_addr) } != 0;
        check!("dma-ring", dma_roundtrip && dma_released && dma_stale, 16);

        report.group("summary");
        let all_ok = report.summary() && failed == 0;
        report.verdict(all_ok);
        failed as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_ok_passes() {
        assert!(data_ok());
    }

    #[test]
    fn machine_check_accepts_sparse_hart_ids() {
        assert!(machine_ok(2, true));
    }

    #[test]
    fn machine_check_rejects_missing_boot_hart() {
        assert!(!machine_ok(2, false));
    }
}
