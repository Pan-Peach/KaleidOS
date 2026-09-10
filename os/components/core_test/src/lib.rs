//! CoreTest 测试组件（第一个 .kcomp）：核内自检 Core 的真实接口。
//!
//! - `kcomp_init`：loader 放段 + 重定位后调用；返回 0 = 全部通过，
//!   非 0 = 失败位图（`load` 命令会据此报告 FAILED）
//! - 自检项（核内，QEMU `load core_test` 时真实执行）：
//!   `.data` 段搬运 / 机器真相 / 内存分配器 / 组件注册表 /
//!   **C4 执行链**（组件加载 → 接口发布/绑定 → 任务创建 → RR 调度 →
//!   上下文切换 → yield/exit → 状态验证）
//! - 只走导出白名单（`kcore_*`），无 god-mode；不直接触碰 TaskTable /
//!   Registry / Sv39 / CpuContext —— 那是 Core 的真相
//! - host 测试只覆盖纯逻辑；真实执行在核内

#![no_std]

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
fn machine_ok(cpu_count: usize, boot_hart_present: bool) -> bool {
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
        fn machine_boot_hart() -> usize;
        #[link_name = "kcore_machine_cpu_count"]
        fn machine_cpu_count() -> usize;
        #[link_name = "kcore_machine_has_hart"]
        fn machine_has_hart(hart_id: usize) -> i32;
        #[link_name = "kcore_free_page_count"]
        fn free_page_count() -> usize;
        #[link_name = "kcore_component_count"]
        fn component_count() -> usize;

        // C4：组件生命周期 / 接口 / 任务 / 调度
        #[link_name = "kcore_component_load"]
        fn component_load(name: *const u8, len: usize) -> i32;
        #[link_name = "kcore_interface_available"]
        fn interface_available(name: *const u8, len: usize, kind: u32, version: u32) -> i32;
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
    }

    // ABI 编码常量（与 Core export.rs 一致）。
    const KIND_POLICY: u32 = 2;
    const STATE_EXITED: i32 = 4;

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
            interface_available(b"scheduler".as_ptr(), b"scheduler".len(), KIND_POLICY, 1) == 1
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
            interface_available(b"scheduler".as_ptr(), b"scheduler".len(), KIND_POLICY, 1) == 1
        };
        check!("scheduler-rr", rr_ready && rr_id >= 0, 9);

        // TODO(C5): 抢占链用例——两个"不 yield 的忙循环"任务被时钟强行切出
        //   （当前调度是协作式；timer 实现 + sched::on_timer_tick 接线后，
        //   在 scheduling chain 组追加 preempt check）。

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
