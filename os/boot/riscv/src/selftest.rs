use arch::{CpuArch, InterruptController, ResetType, SystemReset, Timer};
use core::sync::atomic::{AtomicUsize, Ordering};
use core::{arch::global_asm, mem::MaybeUninit};
use kernel::machine::{IoSpace, MachineInfo};

const STACK_BYTES: usize = 4096;
const UNMAPPED_ADDRESS: usize = 0x4000_0000;

#[cfg(target_arch = "riscv32")]
const PTE_READ_ONLY: u32 = 0xc3;
#[cfg(target_arch = "riscv32")]
const PTE_READ_WRITE: u32 = 0xc7;

/// TLB 用例的 VA：页对齐、Sv32/Sv39 都合法，且在长期地址空间里未映射
/// （与 `UNMAPPED_ADDRESS` 同一空洞；每个 case 跑在独立 QEMU 进程里）。
const TLB_TEST_ADDRESS: usize = 0x4000_0000;
/// Sv32 与 Sv39 的 PTE 低 8 位布局相同；TLB 用例只用到这几个叶子位。
const PTE_V: usize = 1 << 0;
const PTE_R: usize = 1 << 1;
const PTE_W: usize = 1 << 2;
const PTE_A: usize = 1 << 6;
const PTE_D: usize = 1 << 7;
const PTE_LEAF_RW: usize = PTE_V | PTE_R | PTE_W | PTE_A | PTE_D;

#[repr(align(16))]
struct Stack([u8; STACK_BYTES]);

static MAPPING_VALUE: usize = 0x4b41_4c45;
static RODATA_TARGET: u8 = 0x5a;
static mut NX_TARGET: u8 = 0xa5;
static mut STACK_A: Stack = Stack([0; STACK_BYTES]);
static mut STACK_B: Stack = Stack([0; STACK_BYTES]);
#[unsafe(no_mangle)]
static mut SELFTEST_CONTEXT_A: MaybeUninit<arch::ContextImpl> = MaybeUninit::uninit();
#[unsafe(no_mangle)]
static mut SELFTEST_CONTEXT_B: MaybeUninit<arch::ContextImpl> = MaybeUninit::uninit();
#[unsafe(no_mangle)]
static mut SELFTEST_RETURN_CONTEXT: MaybeUninit<arch::ContextImpl> = MaybeUninit::uninit();
#[unsafe(no_mangle)]
static mut SELFTEST_A_SP: usize = 0;
#[unsafe(no_mangle)]
static mut SELFTEST_B_SP: usize = 0;
#[unsafe(no_mangle)]
static mut SELFTEST_A_RESUMED_SP: usize = 0;
#[unsafe(no_mangle)]
static mut SELFTEST_B_RESUMED_SP: usize = 0;
#[unsafe(no_mangle)]
static mut SELFTEST_A_S: [usize; 12] = [0; 12];
#[unsafe(no_mangle)]
static mut SELFTEST_B_S: [usize; 12] = [0; 12];
static TIMER_HANDLER_COUNT: AtomicUsize = AtomicUsize::new(0);
static EXTERNAL_IRQ_COUNT: AtomicUsize = AtomicUsize::new(0);
static EXTERNAL_IRQ_LINE: AtomicUsize = AtomicUsize::new(0);
/// 已发现 UART 的 IER 地址（handler 里要关掉中断源，避免 complete 后立刻重挂）。
static UART_IER_ADDR: AtomicUsize = AtomicUsize::new(0);

#[cfg(target_arch = "riscv32")]
static mut SV32_TEST_TABLE: [u32; 1024] = [0; 1024];

/// TLB 用例的两个物理后备页：各自独占一页，测试只比较第一字节。
#[repr(align(4096))]
struct TestPage([u8; 4096]);
static mut TLB_PAGE_A: TestPage = TestPage([0; 4096]);
static mut TLB_PAGE_B: TestPage = TestPage([0; 4096]);

/// TLB 用例自己 poke 的页表页（纯测试代码，不进生产 arch）。
#[cfg(target_arch = "riscv32")]
#[repr(align(4096))]
struct Sv32TestTable([u32; 1024]);
#[cfg(target_arch = "riscv32")]
static mut TLB_TABLE32: Sv32TestTable = Sv32TestTable([0; 1024]);

#[cfg(target_arch = "riscv64")]
#[repr(align(4096))]
struct Sv39TestTable([u64; 512]);
#[cfg(target_arch = "riscv64")]
static mut TLB_L2: Sv39TestTable = Sv39TestTable([0; 512]);
#[cfg(target_arch = "riscv64")]
static mut TLB_L1: Sv39TestTable = Sv39TestTable([0; 512]);

#[cfg(target_arch = "riscv64")]
global_asm!(
    r#"
.global selftest_context_a_entry
selftest_context_a_entry:
    li s0, 0x11; li s1, 0x12; li s2, 0x13; li s3, 0x14
    li s4, 0x15; li s5, 0x16; li s6, 0x17; li s7, 0x18
    li s8, 0x19; li s9, 0x1a; li s10, 0x1b; li s11, 0x1c
    la t0, SELFTEST_A_SP; sd sp, 0(t0)
    la a0, SELFTEST_CONTEXT_A; la a1, SELFTEST_CONTEXT_B
    call __switch
    la t0, SELFTEST_A_RESUMED_SP; sd sp, 0(t0)
    la t0, SELFTEST_A_S
    sd s0, 0(t0); sd s1, 8(t0); sd s2, 16(t0); sd s3, 24(t0)
    sd s4, 32(t0); sd s5, 40(t0); sd s6, 48(t0); sd s7, 56(t0)
    sd s8, 64(t0); sd s9, 72(t0); sd s10, 80(t0); sd s11, 88(t0)
    tail selftest_context_a_resumed

.global selftest_context_b_entry
selftest_context_b_entry:
    li s0, 0x21; li s1, 0x22; li s2, 0x23; li s3, 0x24
    li s4, 0x25; li s5, 0x26; li s6, 0x27; li s7, 0x28
    li s8, 0x29; li s9, 0x2a; li s10, 0x2b; li s11, 0x2c
    la t0, SELFTEST_B_SP; sd sp, 0(t0)
    la a0, SELFTEST_CONTEXT_B; la a1, SELFTEST_CONTEXT_A
    call __switch
    la t0, SELFTEST_B_RESUMED_SP; sd sp, 0(t0)
    la t0, SELFTEST_B_S
    sd s0, 0(t0); sd s1, 8(t0); sd s2, 16(t0); sd s3, 24(t0)
    sd s4, 32(t0); sd s5, 40(t0); sd s6, 48(t0); sd s7, 56(t0)
    sd s8, 64(t0); sd s9, 72(t0); sd s10, 80(t0); sd s11, 88(t0)
    tail selftest_context_b_resumed
"#
);

#[cfg(target_arch = "riscv32")]
global_asm!(
    r#"
.global selftest_context_a_entry
selftest_context_a_entry:
    li s0, 0x11; li s1, 0x12; li s2, 0x13; li s3, 0x14
    li s4, 0x15; li s5, 0x16; li s6, 0x17; li s7, 0x18
    li s8, 0x19; li s9, 0x1a; li s10, 0x1b; li s11, 0x1c
    la t0, SELFTEST_A_SP; sw sp, 0(t0)
    la a0, SELFTEST_CONTEXT_A; la a1, SELFTEST_CONTEXT_B
    call __switch
    la t0, SELFTEST_A_RESUMED_SP; sw sp, 0(t0)
    la t0, SELFTEST_A_S
    sw s0, 0(t0); sw s1, 4(t0); sw s2, 8(t0); sw s3, 12(t0)
    sw s4, 16(t0); sw s5, 20(t0); sw s6, 24(t0); sw s7, 28(t0)
    sw s8, 32(t0); sw s9, 36(t0); sw s10, 40(t0); sw s11, 44(t0)
    tail selftest_context_a_resumed

.global selftest_context_b_entry
selftest_context_b_entry:
    li s0, 0x21; li s1, 0x22; li s2, 0x23; li s3, 0x24
    li s4, 0x25; li s5, 0x26; li s6, 0x27; li s7, 0x28
    li s8, 0x29; li s9, 0x2a; li s10, 0x2b; li s11, 0x2c
    la t0, SELFTEST_B_SP; sw sp, 0(t0)
    la a0, SELFTEST_CONTEXT_B; la a1, SELFTEST_CONTEXT_A
    call __switch
    la t0, SELFTEST_B_RESUMED_SP; sw sp, 0(t0)
    la t0, SELFTEST_B_S
    sw s0, 0(t0); sw s1, 4(t0); sw s2, 8(t0); sw s3, 12(t0)
    sw s4, 16(t0); sw s5, 20(t0); sw s6, 24(t0); sw s7, 28(t0)
    sw s8, 32(t0); sw s9, 36(t0); sw s10, 40(t0); sw s11, 44(t0)
    tail selftest_context_b_resumed
"#
);

unsafe extern "C" {
    fn selftest_context_a_entry();
    fn selftest_context_b_entry();
}

/// 读取用例名并分发。**在完整初始化（core + runtime VM）之后调用**——
/// device MMIO 已映射，所以外部中断这类用例能真碰硬件。
pub fn run(info: &MachineInfo) -> ! {
    kernel::log!("selftest", "ready");
    let mut command = [0u8; 64];
    let length = kernel::print::read_line(&mut command);
    kernel::printk!("\n");
    match &command[..length] {
        b"mapping" => mapping(),
        b"context-switch" => context_switch(),
        b"panic-containment" => panic_containment(),
        b"task-panic" => task_panic(),
        b"panic-component" => panic_component(),
        b"illegal-instruction" => illegal_instruction(),
        b"load-fault" => load_fault(),
        b"store-readonly" => store_readonly_fault(),
        b"execute-nx" => execute_nx_fault(),
        b"tlb-flush" => tlb_flush(),
        b"tlb-invalidate" => tlb_invalidate(),
        b"breakpoint" => breakpoint_fault(),
        b"timer" => timer(),
        b"external-irq" => external_irq(info),
        // increment 3：私有 AS assembly gateway（机制证明；组件生命周期未接线）。
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-transition" => isolated_tests::isolated_transition(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-timer" => isolated_tests::isolated_timer(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-fault" => isolated_tests::isolated_fault(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-fault-abandon" => isolated_tests::isolated_fault_abandon(),
        // increment 4：真实 `.kcomp` 的按域装载 + 页级权限强制（仍 inactive path）。
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-image" => isolated_tests::isolated_image(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-image-wrong-env" => isolated_tests::isolated_image_wrong_env(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-perm-text" => isolated_tests::isolated_perm_text(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-perm-data" => isolated_tests::isolated_perm_data(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-core-unreachable" => isolated_tests::isolated_core_unreachable(),
        // increment 5：Isolated 生命周期接线（生产 create → Ready → destroy）。
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-lifecycle" => isolated_tests::isolated_lifecycle(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-lifecycle-fail" => isolated_tests::isolated_lifecycle_fail(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-lifecycle-fault" => isolated_tests::isolated_lifecycle_fault(),
        _ => fail("unknown command"),
    }
}

fn panic_containment() -> ! {
    use kernel::component::containment::{call_component_create, CallOutcome, KcompCreateArgs};
    let args = KcompCreateArgs::empty();
    let mut state: *mut () = core::ptr::null_mut();
    match call_component_create(
        panic_containment_entry as *const () as usize,
        &args,
        &mut state,
    ) {
        CallOutcome::Panicked => pass("panic-containment"),
        CallOutcome::Returned(_) => fail("component panic returned unexpectedly"),
        CallOutcome::NoStack => fail("boundary stack allocation failed"),
    }
}

extern "C" fn panic_containment_entry(
    _args: *const kernel::component::containment::KcompCreateArgs,
    _out_state: *mut *mut (),
) -> i32 {
    panic!("component panic test");
}

/// Task-boundary containment ArchTest.
///
/// Loads a real scheduler policy provider plus a separate victim component from
/// the embedded archive, then creates two component-owned tasks directly in the
/// Core table (white-box: task entries live in the boot image, so the
/// image-range check in `task::create_task` is intentionally bypassed).  One
/// task panics; the scheduler's ambient escape guard must redirect it to the
/// Core abort trampoline, which kills the task, fails the victim component, and
/// reschedules so the normal task still completes and Core survives.
fn task_panic() -> ! {
    // A Ready scheduler policy provider: load the reference implementation, then
    // **explicitly** discover its `scheduler.policy` endpoint and select it —
    // Core's scheduling path never discovers a scheduler by name.
    let provider = match kernel::component::load::load_and_start(
        b"scheduler_rr",
        kernel::component::endpoint::ExecutionDomain::KernelNative,
    ) {
        Ok(id) => id,
        Err(_) => fail("task-panic: scheduler_rr load failed"),
    };
    if kernel::sched::select_provider(provider).is_err() {
        fail("task-panic: scheduler policy selection failed");
    }
    // A separate Ready victim, so failing it does not remove the scheduler.
    let victim = match kernel::component::load::load_and_start(
        b"kcomp_smoke",
        kernel::component::endpoint::ExecutionDomain::KernelNative,
    ) {
        Ok(id) => id,
        Err(_) => fail("task-panic: kcomp_smoke load failed"),
    };

    let (panicking, normal) = {
        let mut table = kernel::task::get_task_table().lock();
        let Ok(panicking) = table.create(
            victim,
            task_panic_entry as *const () as usize,
            core::ptr::null_mut(),
        ) else {
            fail("task-panic: panic task create failed");
        };
        let Ok(normal) = table.create(
            victim,
            task_normal_entry as *const () as usize,
            core::ptr::null_mut(),
        ) else {
            fail("task-panic: normal task create failed");
        };
        if table
            .transition(panicking, kernel::task::TaskState::Runnable)
            .is_err()
            || table
                .transition(normal, kernel::task::TaskState::Runnable)
                .is_err()
        {
            fail("task-panic: task start failed");
        }
        (panicking, normal)
    };

    // Drive the real scheduler on the anchor.  The panicking task escapes into
    // the Core abort trampoline; the normal task runs and exits normally.
    if kernel::sched::run().is_err() {
        fail("task-panic: scheduler run failed");
    }

    let (panicking_state, normal_state) = {
        let table = kernel::task::get_task_table().lock();
        (
            table.get(panicking).map(|record| record.state()),
            table.get(normal).map(|record| record.state()),
        )
    };
    let victim_failed = kernel::component::registry::get_registry()
        .lock()
        .get(victim)
        .map(|record| record.state)
        == Some(kernel::component::ComponentState::Failed);

    let contained = panicking_state == Some(kernel::task::TaskState::Exited)
        && normal_state == Some(kernel::task::TaskState::Exited)
        && victim_failed;
    if contained {
        pass("task-panic")
    } else {
        fail("task-panic: panic was not contained")
    }
}

extern "C" fn task_panic_entry(_arg: *mut ()) -> ! {
    panic!("component task panic");
}

/// step 2 D：**真实 `.kcomp`** 的 panic containment。
///
/// 从内嵌 kpkg 加载 `kcomp_panic`。它的 `kcomp_instance_create` 刻意 `panic!`，
/// 进入的是**组件镜像自己的** SDK panic adapter（不是 boot panic handler）；
/// adapter 经 `kcore_log_line` 打印诊断后调 `kcore_panic_escape`，逃逸回 Core 的
/// create containment 边界。这里断言：Core 存活、该 instance 被提交为 Failed；
/// 诊断行由 `arch_runner.py` 在串口输出上断言。
fn panic_component() -> ! {
    use kernel::component::load::ComponentLoadError;
    match kernel::component::load::load_and_start(
        b"kcomp_panic",
        kernel::component::endpoint::ExecutionDomain::KernelNative,
    ) {
        Err(ComponentLoadError::CreatePanicked) => {}
        Ok(_) => fail("panic-component: component did not panic"),
        Err(_) => fail("panic-component: unexpected load error"),
    }
    let failed = {
        let reg = kernel::component::registry::get_registry().lock();
        let images = kernel::component::image::get_images().lock();
        // 先取出 bool，避免块尾表达式把 `reg` 的借用拖过局部变量析构。
        let any = reg.iter().any(|record| {
            images
                .get(record.image)
                .is_some_and(|img| img.name.as_slice() == b"kcomp_panic")
                && record.state == kernel::component::ComponentState::Failed
        });
        any
    };
    if failed {
        pass("panic-component")
    } else {
        fail("panic-component: instance not marked Failed")
    }
}

extern "C" fn task_normal_entry(_arg: *mut ()) -> ! {
    let _ = kernel::sched::exit_current();
    loop {
        core::hint::spin_loop();
    }
}

/// 按 compatible（任一命中）找已发现设备的 MMIO 窗口。
fn find_mmio(info: &MachineInfo, compatibles: &[&[u8]]) -> Option<(usize, usize)> {
    info.devices[..info.dev_count].iter().find_map(|device| {
        let hit = device.compatibles[..device.compat_count as usize]
            .iter()
            .any(|c| compatibles.contains(&c.as_str().as_bytes()));
        if !hit {
            return None;
        }
        match device.space {
            IoSpace::Mmio { base, size } => Some((base, size)),
            IoSpace::Pio { .. } => None,
        }
    })
}

/// 按 compatible（任一命中）找设备的 PLIC 中断号。
fn find_irq(info: &MachineInfo, compatibles: &[&[u8]]) -> Option<u32> {
    info.devices[..info.dev_count].iter().find_map(|device| {
        let hit = device.compatibles[..device.compat_count as usize]
            .iter()
            .any(|c| compatibles.contains(&c.as_str().as_bytes()));
        if !hit {
            return None;
        }
        device.irq
    })
}

fn mapping() -> ! {
    let high = core::ptr::addr_of!(MAPPING_VALUE) as usize;
    let low = arch::physical_address_of(high);
    // SAFETY: the boot identity alias of this linked static is mapped.
    let value = unsafe { core::ptr::read_volatile(low as *const usize) };
    if value != MAPPING_VALUE {
        fail("mapping read-back mismatch");
    }
    #[cfg(target_arch = "riscv64")]
    if high == low {
        fail("high-half alias missing");
    }
    pass("mapping")
}

fn illegal_instruction() -> ! {
    // The all-zero instruction encoding is reserved and must trap as illegal.
    unsafe { core::arch::asm!(".word 0", options(noreturn)) }
}

fn load_fault() -> ! {
    #[cfg(target_arch = "riscv32")]
    unsafe {
        // SAFETY: this invalidates the unused root entry immediately before use.
        rv32_unmap(UNMAPPED_ADDRESS);
    }
    // SAFETY: the CPU faults before this unmapped pointer can be dereferenced.
    unsafe { core::ptr::read_volatile(UNMAPPED_ADDRESS as *const u8) };
    fail("unmapped load returned")
}

fn store_readonly_fault() -> ! {
    #[cfg(target_arch = "riscv32")]
    unsafe {
        // SAFETY: only this page loses write permission for the destructive test.
        rv32_set_page_permissions(core::ptr::addr_of!(RODATA_TARGET) as usize, PTE_READ_ONLY);
    }
    // SAFETY: the read-only mapping faults before the immutable static mutates.
    unsafe { core::ptr::write_volatile(core::ptr::addr_of!(RODATA_TARGET) as *mut u8, 0) };
    fail("read-only store returned")
}

fn execute_nx_fault() -> ! {
    #[cfg(target_arch = "riscv32")]
    unsafe {
        // SAFETY: only this page loses execute permission for the destructive test.
        rv32_set_page_permissions(core::ptr::addr_of_mut!(NX_TARGET) as usize, PTE_READ_WRITE);
    }
    let address = core::ptr::addr_of_mut!(NX_TARGET) as usize;
    // SAFETY: instruction fetch faults on the NX mapping before this data is run.
    let entry: unsafe extern "C" fn() = unsafe { core::mem::transmute(address) };
    unsafe { entry() };
    fail("NX execute returned")
}

/// TLB flush ArchTest（docs/development/testing.md §2「TLB flush 是否正确」）。
///
/// 在**活动** satp 页表里把同一个 VA 依次改指到两个不同的物理页，每次改完
/// `sfence.vma`，然后读 VA：必须读到**新**后备页的标记。关键在中间那一步——
/// 第一次读已经把旧翻译填进 TLB，若改 PTE 后 flush 失效，第二次读会命中旧项、
/// 读到旧页的 0xa1；只有真的走了页表才可能读到 0xb2。
///
/// 页表由本用例自己 poke（白盒测试代码），RV32/RV64 各一份机制；生产 arch
/// 不动。
fn tlb_flush() -> ! {
    // SAFETY: 两个静态测试页都活着且只被本用例使用。
    let (pa_a, pa_b) = unsafe {
        (
            test_page_pa(core::ptr::addr_of_mut!(TLB_PAGE_A)),
            test_page_pa(core::ptr::addr_of_mut!(TLB_PAGE_B)),
        )
    };
    // SAFETY: 两个后备页在 identity 映射的 RAM 里；VA 是测试专用空洞，
    // 每次 poke 后立即 sfence.vma，旧翻译不会再被合法使用。
    unsafe {
        core::ptr::write_volatile(pa_a as *mut u8, 0xa1);
        core::ptr::write_volatile(pa_b as *mut u8, 0xb2);

        selftest_map_page(TLB_TEST_ADDRESS, pa_a, PTE_LEAF_RW);
        if core::ptr::read_volatile(TLB_TEST_ADDRESS as *const u8) != 0xa1 {
            fail("tlb-flush: first translation did not take effect");
        }

        selftest_map_page(TLB_TEST_ADDRESS, pa_b, PTE_LEAF_RW);
        if core::ptr::read_volatile(TLB_TEST_ADDRESS as *const u8) != 0xb2 {
            fail("tlb-flush: stale translation survived sfence.vma");
        }

        selftest_map_page(TLB_TEST_ADDRESS, pa_a, PTE_LEAF_RW);
        if core::ptr::read_volatile(TLB_TEST_ADDRESS as *const u8) != 0xa1 {
            fail("tlb-flush: remap back did not take effect");
        }
    }
    pass("tlb-flush")
}

/// TLB invalidation ArchTest：**先访问**建好的翻译（灌 TLB），再清掉 PTE 并
/// `sfence.vma`；此后访问必须由硬件 fault（fault-based，runner 期待 scause 13）。
/// 若 flush 缺失，访问会命中 TLB 旧项、把已撤销页读回来，落到 `fail`。
fn tlb_invalidate() -> ! {
    // SAFETY: 静态测试页活着且只被本用例使用。
    let pa_a = unsafe { test_page_pa(core::ptr::addr_of_mut!(TLB_PAGE_A)) };
    // SAFETY: 同 `tlb_flush`；最后一次访问必须 fault，不会返回这里。
    unsafe {
        core::ptr::write_volatile(pa_a as *mut u8, 0xa1);
        selftest_map_page(TLB_TEST_ADDRESS, pa_a, PTE_LEAF_RW);
        if core::ptr::read_volatile(TLB_TEST_ADDRESS as *const u8) != 0xa1 {
            fail("tlb-invalidate: translation did not take effect");
        }
        selftest_map_page(TLB_TEST_ADDRESS, 0, 0);
        // SAFETY: 映射已撤销且已 sfence，这次 load 必须触发 load page fault。
        core::ptr::read_volatile(TLB_TEST_ADDRESS as *const u8);
    }
    fail("tlb-invalidate: stale translation survived invalidation")
}

/// `ebreak` → scause 3（Breakpoint）。OpenSBI 把 breakpoint 异常委托给
/// S-mode（`medeleg` bit 3），所以 S-mode 执行 `ebreak` 必须进本内核的 trap
/// 入口；runner 断言 `scause=0x3`。
fn breakpoint_fault() -> ! {
    // SAFETY: 委托成立时 trap handler 接管且永不返回；若平台不委托，
    // 执行流落回下一行进入 fail（而不是 UB）。
    unsafe { core::arch::asm!("ebreak") };
    fail("breakpoint returned")
}

/// 定时器 ArchTest（C5 骨架位）：N tick 窗口内时钟中断确实触发，且 `sret`
/// 返回后被打断的现场（寄存器/栈）不破坏。
///
/// 本用例走裸 arch（不依赖 Core）：设置一个 one-shot deadline，等待一次
/// timer IRQ，由 handler disarm，然后继续等待一个时间窗口确认没有重复 IRQ。
fn timer() -> ! {
    const WINDOW: u64 = 10_000;

    TIMER_HANDLER_COUNT.store(0, Ordering::Release);
    arch::TimerImpl::register_timer_handler(timer_handler);
    arch::TimerImpl::enable_timer_interrupt();

    let first_deadline = arch::TimerImpl::now().saturating_add(WINDOW);
    arch::TimerImpl::set_deadline(first_deadline);

    while TIMER_HANDLER_COUNT.load(Ordering::Acquire) == 0 {
        core::hint::spin_loop();
    }

    let quiet_until = arch::TimerImpl::now().saturating_add(WINDOW);
    while arch::TimerImpl::now() < quiet_until {
        core::hint::spin_loop();
    }

    if TIMER_HANDLER_COUNT.load(Ordering::Acquire) != 1 {
        kernel::log!(
            "selftest",
            "timer count={} now={} quiet_until={}",
            TIMER_HANDLER_COUNT.load(Ordering::Acquire),
            arch::TimerImpl::now(),
            quiet_until
        );
        fail("timer delivered more than one interrupt");
    }
    pass("timer")
}

extern "C" fn timer_handler() {
    TIMER_HANDLER_COUNT.fetch_add(1, Ordering::AcqRel);
    arch::TimerImpl::cancel_deadline();
}

/// C6 外部中断 ArchTest：PLIC 真的把一条设备线投递到 S-mode handler。
///
/// 触发源用 **UART 的 THRE**（发送保持寄存器空）：打开 `IER.THRE` 后 UART 立刻
/// 拉高中断线，不需要 runner 注入串口输入。handler 里先关掉 UART 中断源——THRE
/// 是电平触发，不关的话 claim/complete 之后马上又 pending（中断风暴）。
///
/// 本用例直接驱动 PLIC（arch 白盒），不经过 Core 的 `irq::route`；Core 路由由
/// host 测试与 CoreTest 覆盖。
fn external_irq(info: &MachineInfo) -> ! {
    // QEMU virt：PLIC + ns16550a（UART）；UART 的 IER 在 base+1（reg-shift 0）。
    const UART_IER_OFFSET: usize = 1;
    const IER_THRE: u8 = 0x02;
    const WAIT_TICKS: u64 = 10_000_000;

    let (plic_base, _) = find_mmio(
        info,
        &[b"riscv,plic0".as_slice(), b"sifive,plic-1.0.0".as_slice()],
    )
    .expect("PLIC device not found");
    let (uart_base, _) = find_mmio(info, &[b"ns16550a".as_slice()]).expect("UART device not found");
    let uart_line = find_irq(info, &[b"ns16550a".as_slice()]).expect("UART irq not found");
    let ier = uart_base + UART_IER_OFFSET;

    <arch::InterruptImpl as arch::InterruptController>::configure(plic_base, info.boot_hart);
    arch::InterruptImpl::register_external_handler(external_irq_handler);
    arch::InterruptImpl::enable(uart_line);

    EXTERNAL_IRQ_COUNT.store(0, Ordering::Release);
    EXTERNAL_IRQ_LINE.store(0, Ordering::Release);
    UART_IER_ADDR.store(ier, Ordering::Release);

    // 打开 UART THRE 中断：THR 空 → UART 立刻断言中断线（无需外部输入）。
    // SAFETY: ier 是已发现 UART 的寄存器地址（字节宽 IER）。
    unsafe { core::ptr::write_volatile(ier as *mut u8, IER_THRE) };
    arch::InterruptImpl::enable_external_interrupt();

    let deadline = arch::TimerImpl::now().saturating_add(WAIT_TICKS);
    while EXTERNAL_IRQ_COUNT.load(Ordering::Acquire) == 0 && arch::TimerImpl::now() < deadline {
        core::hint::spin_loop();
    }

    let count = EXTERNAL_IRQ_COUNT.load(Ordering::Acquire);
    let line = EXTERNAL_IRQ_LINE.load(Ordering::Acquire);
    if count != 1 || line != uart_line as usize {
        kernel::log!(
            "selftest",
            "external-irq count={} line={} want_line={}",
            count,
            line,
            uart_line
        );
        fail("external IRQ was not delivered exactly once from the UART line");
    }
    pass("external-irq")
}

extern "C" fn external_irq_handler() {
    // 顺序要紧：**先 claim 再关源**。claim 读走 pending 并置 in-service、才拿得到 id；
    // 若先关设备（UART 电平触发），pending 随电平撤销，claim 会返回 0。
    if let Some(line) = arch::InterruptImpl::claim() {
        EXTERNAL_IRQ_LINE.store(line as usize, Ordering::Release);
        // 关中断源（THRE 电平触发：不关的话 complete 后立刻又 pending = 风暴）
        let ier = UART_IER_ADDR.load(Ordering::Acquire);
        if ier != 0 {
            // SAFETY: 已发现 UART 的 IER 寄存器（字节宽）。
            unsafe { core::ptr::write_volatile(ier as *mut u8, 0) };
        }
        arch::InterruptImpl::complete(line);
    }
    EXTERNAL_IRQ_COUNT.fetch_add(1, Ordering::AcqRel);
}

// ---------------------------------------------------------------------------
// increment 3/4：Isolated 域 ArchTest（私有 AS 切换 / trap 往返 / 组件故障分派 /
// 按域装载 + 页级权限强制）。机制已落地但**生命周期未接线**：这里直接驱动
// Core 准备 + arch 汇编 + `component::isolated_load`，证明机制本身。
// ---------------------------------------------------------------------------
#[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
mod isolated_tests {
    use super::{fail, pass};
    use arch::Timer;
    use core::arch::global_asm;
    use core::sync::atomic::{AtomicUsize, Ordering};
    use kernel::component::isolated::{
        self, ComponentFault, FaultDecision, IsolatedPrepareError, Outcome, PreparedTransition,
    };
    use kernel::component::isolated_load::{self, PlacedImage, PlacedSegment};
    use kernel::component::ComponentId;
    use kernel::memory::address_space::{
        self, AddressSpaceHandle, Mapping, MappingPermission, PhysicalRange, VirtualRange,
    };

    #[cfg(target_arch = "riscv64")]
    global_asm!(include_str!("isolated64.S"));
    #[cfg(target_arch = "riscv32")]
    global_asm!(include_str!("isolated32.S"));

    unsafe extern "C" {
        static isolated_fixture_start: u8;
        static isolated_fixture_end: u8;
        fn isolated_roundtrip_entry();
        fn isolated_timer_entry();
        fn isolated_fault_entry();
        fn isolated_abandon_entry();
        fn isolated_roundtrip_probe();
    }

    /// 实例 AS 的 VA 布局（与 `isolated{64,32}.S` 里的常量一致）。
    const ISOLATED_CTL_VA: usize = 0x3000_0000;
    const ISOLATED_DATA_VA: usize = 0x3000_1000;
    const ISOLATED_STACK_BASE: usize = 0x3000_2000;
    const ISOLATED_STACK_SIZE: usize = 4096;
    const ISOLATED_ABANDON_VA: usize = 0x5000_0000;

    /// 控制页槽号（字节偏移 = 槽号 × `size_of::<usize>()`）。
    const CTL_MAGIC: usize = 0;
    const CTL_SATP: usize = 1;
    const CTL_FLAG: usize = 2;
    const CTL_ITER: usize = 3;
    const CTL_DATA: usize = 4;

    /// 预置在惰性数据页里的值（fault 恢复后组件读到并写回控制页）。
    const DATA_MAGIC: usize = 0x4d41_4749; // "MAGI"

    #[unsafe(no_mangle)]
    static mut ISOLATED_PENDING: Option<PreparedTransition> = None;
    static ISOLATED_CTL_PA: AtomicUsize = AtomicUsize::new(0);
    static ISOLATED_DATA_PA: AtomicUsize = AtomicUsize::new(0);
    static ISOLATED_HANDLE_ID: AtomicUsize = AtomicUsize::new(0);
    static ISOLATED_HANDLE_GENERATION: AtomicUsize = AtomicUsize::new(0);
    static ISOLATED_INSTANCE_SATP: AtomicUsize = AtomicUsize::new(0);
    static ISOLATED_CORE_SATP_BEFORE: AtomicUsize = AtomicUsize::new(0);
    static ISOLATED_CORE_SATP_AFTER: AtomicUsize = AtomicUsize::new(0);
    static ISOLATED_OUTCOME: AtomicUsize = AtomicUsize::new(0);
    static ISOLATED_FAULTED: AtomicUsize = AtomicUsize::new(0);

    #[unsafe(no_mangle)]
    static mut ISOLATED_S_REGS: [usize; 12] = [0; 12];
    #[unsafe(no_mangle)]
    static mut ISOLATED_TP_AFTER: usize = 0;
    #[unsafe(no_mangle)]
    static mut ISOLATED_GP_BEFORE: usize = 0;
    #[unsafe(no_mangle)]
    static mut ISOLATED_GP_MATCH: usize = 0;

    static TIMER_COUNT: AtomicUsize = AtomicUsize::new(0);
    static TIMER_HANDLER_SATP: AtomicUsize = AtomicUsize::new(0);
    static TIMER_HANDLER_SP: AtomicUsize = AtomicUsize::new(0);

    static FAULT_COUNT: AtomicUsize = AtomicUsize::new(0);
    static FAULT_TARGET_VA: AtomicUsize = AtomicUsize::new(0);
    static FAULT_TARGET_PA: AtomicUsize = AtomicUsize::new(0);
    static FAULT_HANDLER_SATP: AtomicUsize = AtomicUsize::new(0);
    static FAULT_HANDLER_SP: AtomicUsize = AtomicUsize::new(0);
    static FAULT_SEPC: AtomicUsize = AtomicUsize::new(0);

    fn read_satp() -> usize {
        let satp: usize;
        // SAFETY: CSR read only; no memory / stack effects.
        unsafe {
            core::arch::asm!(
                "csrr {satp}, satp",
                satp = out(reg) satp,
                options(nostack, preserves_flags),
            );
        }
        satp
    }

    fn read_sp() -> usize {
        let sp: usize;
        // SAFETY: register move only.
        unsafe {
            core::arch::asm!(
                "mv {sp}, sp",
                sp = out(reg) sp,
                options(nomem, nostack, preserves_flags),
            );
        }
        sp
    }

    fn ctl_pa() -> *mut usize {
        ISOLATED_CTL_PA.load(Ordering::Acquire) as *mut usize
    }

    unsafe fn ctl_word(index: usize) -> usize {
        // SAFETY: caller guarantees the fixture was set up; identity PA view.
        unsafe { ctl_pa().add(index).read() }
    }

    unsafe fn ctl_set(index: usize, value: usize) {
        // SAFETY: 同上。
        unsafe { ctl_pa().add(index).write(value) };
    }

    fn take_pending() -> PreparedTransition {
        // SAFETY: single-threaded selftest; the probe/driver runs exactly once.
        let pending = unsafe { (*core::ptr::addr_of_mut!(ISOLATED_PENDING)).take() };
        match pending {
            Some(transition) => transition,
            None => fail("isolated: pending transition missing"),
        }
    }

    fn prepare_or_fail(
        handle: AddressSpaceHandle,
        entry: usize,
        interrupts_enabled: bool,
    ) -> PreparedTransition {
        let stack = VirtualRange {
            base: ISOLATED_STACK_BASE,
            size: ISOLATED_STACK_SIZE,
        };
        match isolated::prepare(handle, entry, stack, 0, interrupts_enabled, (0, 0)) {
            Ok(transition) => transition,
            Err(IsolatedPrepareError::NoSuchSpace) => fail("isolated: prepare: no such space"),
            Err(IsolatedPrepareError::Retired) => fail("isolated: prepare: retired space"),
            Err(IsolatedPrepareError::Unsupported) => fail("isolated: prepare: unsupported"),
            Err(IsolatedPrepareError::GatewayMapping) => {
                fail("isolated: prepare: gateway mapping conflict")
            }
            Err(IsolatedPrepareError::EntryNotExecutable) => {
                fail("isolated: prepare: entry not executable")
            }
            Err(IsolatedPrepareError::StackNotWritable) => {
                fail("isolated: prepare: stack not writable")
            }
            Err(IsolatedPrepareError::InvalidStack) => fail("isolated: prepare: invalid stack"),
        }
    }

    struct IsolatedFixture {
        handle: AddressSpaceHandle,
    }

    /// 建立测试实例：私有 AS + 夹具代码页 + 控制页 + 组件栈（DATA 页故意留空）。
    fn isolated_fixture() -> Result<IsolatedFixture, &'static str> {
        if !address_space::isolation_capable() {
            return Err("isolated: this profile has no private address space backend");
        }
        let handle = address_space::create_address_space_for(ComponentId::from_raw(0x150))
            .map_err(|_| "isolated: create_address_space_for failed")?;

        let ctl_pa = kernel::memory::vm_page_alloc().map_err(|_| "isolated: ctl page alloc")?;
        let data_pa = kernel::memory::vm_page_alloc().map_err(|_| "isolated: data page alloc")?;
        let stack_pa = kernel::memory::vm_page_alloc().map_err(|_| "isolated: stack page alloc")?;

        // 夹具代码页：**测试夹具**（不是普通 Core 段），`.S` 用 balign 4096 保证
        // 整页独占；映射进实例 AS 的是同一 VA → 同一 PA。
        let fixture_va = core::ptr::addr_of!(isolated_fixture_start) as usize;
        let fixture_end = core::ptr::addr_of!(isolated_fixture_end) as usize;
        let fixture_size = fixture_end
            .checked_sub(fixture_va)
            .ok_or("isolated: fixture symbols out of order")?;
        if !fixture_va.is_multiple_of(4096) || fixture_size > 4096 {
            return Err("isolated: fixture page must fit in one exclusive page");
        }

        let map_page = |va: usize, pa: usize, permission| {
            address_space::map(
                handle,
                Mapping {
                    virtual_range: VirtualRange {
                        base: va,
                        size: 4096,
                    },
                    physical_range: PhysicalRange {
                        base: pa,
                        size: 4096,
                    },
                    permission,
                },
            )
            .map_err(|_| "isolated: instance mapping failed")
        };
        map_page(
            fixture_va,
            arch::physical_address_of(fixture_va),
            MappingPermission::READ | MappingPermission::EXECUTE,
        )?;
        map_page(
            ISOLATED_CTL_VA,
            ctl_pa,
            MappingPermission::READ | MappingPermission::WRITE,
        )?;
        map_page(
            ISOLATED_STACK_BASE,
            stack_pa,
            MappingPermission::READ | MappingPermission::WRITE,
        )?;
        // ISOLATED_DATA_VA 故意不映射：fault / abandon 用例的"缺失映射"。

        // 经 Core root 的 identity 视图初始化实例页（PA 可直接解引用）。
        // SAFETY: 三个页都来自 vm_page_alloc，identity RAM 映射 RWX。
        unsafe {
            core::ptr::write_bytes(ctl_pa as *mut u8, 0, 4096);
            core::ptr::write_bytes(stack_pa as *mut u8, 0, 4096);
            core::ptr::write_bytes(data_pa as *mut u8, 0, 4096);
            (data_pa as *mut usize).write(DATA_MAGIC);
        }

        ISOLATED_CTL_PA.store(ctl_pa, Ordering::Release);
        ISOLATED_DATA_PA.store(data_pa, Ordering::Release);
        ISOLATED_HANDLE_ID.store(handle.raw_id() as usize, Ordering::Release);
        ISOLATED_HANDLE_GENERATION.store(handle.raw_generation() as usize, Ordering::Release);
        Ok(IsolatedFixture { handle })
    }

    /// 同步往返用例：寄存器纪律探针 → gateway → 组件 → Core 恢复。
    pub(super) fn isolated_transition() -> ! {
        let fixture = match isolated_fixture() {
            Ok(fixture) => fixture,
            Err(reason) => fail(reason),
        };
        // SAFETY: fixture page symbol; only used to form the entry address.
        let entry = isolated_roundtrip_entry as *const () as usize;
        let transition = prepare_or_fail(fixture.handle, entry, false);
        ISOLATED_INSTANCE_SATP.store(transition.satp(), Ordering::Release);
        ISOLATED_OUTCOME.store(0, Ordering::Release);
        ISOLATED_FAULTED.store(0, Ordering::Release);
        // SAFETY: single-threaded; the probe reads it back through the driver.
        unsafe { (*core::ptr::addr_of_mut!(ISOLATED_PENDING)).replace(transition) };
        // SAFETY: the probe sets callee-saved magic, calls the driver (which runs
        // the gateway), then `tail`s to the resumed checker — it never returns.
        unsafe { isolated_roundtrip_probe() };
        fail("isolated-transition: probe returned unexpectedly")
    }

    /// 探针调用的 Rust driver：执行一次切换并记录结果。
    #[unsafe(no_mangle)]
    extern "C" fn isolated_roundtrip_driver() {
        let before = read_satp();
        ISOLATED_CORE_SATP_BEFORE.store(before, Ordering::Release);
        let transition = take_pending();
        let outcome = isolated::enter(transition);
        ISOLATED_CORE_SATP_AFTER.store(read_satp(), Ordering::Release);
        match outcome {
            Outcome::Returned(value) => {
                ISOLATED_OUTCOME.store(value, Ordering::Release);
                ISOLATED_FAULTED.store(0, Ordering::Release);
            }
            Outcome::Faulted => ISOLATED_FAULTED.store(1, Ordering::Release),
        }
    }

    /// 探针 `tail` 到这里（Core 恢复后）：断言恢复纪律与实例侧证据。
    #[unsafe(no_mangle)]
    extern "C" fn selftest_isolated_roundtrip_resumed() -> ! {
        // 1) 同步切换的保存 / 恢复纪律。
        // SAFETY: the probe filled this array before tail-calling here.
        let regs = unsafe { &*core::ptr::addr_of!(ISOLATED_S_REGS) };
        if regs
            .iter()
            .enumerate()
            .any(|(i, value)| *value != 0x101 + i)
        {
            fail("isolated-transition: callee-saved registers not restored");
        }
        // SAFETY: probe-written statics; single-threaded.
        if unsafe { core::ptr::addr_of!(ISOLATED_TP_AFTER).read() } != 0x707 {
            fail("isolated-transition: tp (runtime slot) not restored");
        }
        if unsafe { core::ptr::addr_of!(ISOLATED_GP_MATCH).read() } != 0 {
            fail("isolated-transition: gp not restored");
        }
        if ISOLATED_CORE_SATP_BEFORE.load(Ordering::Acquire)
            != ISOLATED_CORE_SATP_AFTER.load(Ordering::Acquire)
        {
            fail("isolated-transition: Core satp not restored");
        }
        // 2) 组件确实在私有 root 上运行过：控制页的写入只能经实例映射到达。
        if ISOLATED_FAULTED.load(Ordering::Acquire) != 0
            || ISOLATED_OUTCOME.load(Ordering::Acquire) != 0x5a
        {
            fail("isolated-transition: unexpected component outcome");
        }
        // SAFETY: fixture ensured; identity PA view of the control page.
        if unsafe { ctl_word(CTL_MAGIC) } != 0x5151 {
            fail("isolated-transition: component did not write its control page");
        }
        let instance_satp = ISOLATED_INSTANCE_SATP.load(Ordering::Acquire);
        if unsafe { ctl_word(CTL_SATP) } != instance_satp {
            fail("isolated-transition: component did not observe the private root");
        }
        if instance_satp == read_satp() {
            fail("isolated-transition: private root equals the Core root");
        }
        pass("isolated-transition")
    }

    /// timer 用例：组件在私有 AS 里循环，时钟中断在组件上下文被接收，经
    /// Core AS / Core trap 栈处理后再 `sret` 恢复组件。
    pub(super) fn isolated_timer() -> ! {
        let fixture = match isolated_fixture() {
            Ok(fixture) => fixture,
            Err(reason) => fail(reason),
        };
        // SAFETY: fixture page symbol.
        let entry = isolated_timer_entry as *const () as usize;
        let transition = prepare_or_fail(fixture.handle, entry, true);
        ISOLATED_INSTANCE_SATP.store(transition.satp(), Ordering::Release);
        TIMER_COUNT.store(0, Ordering::Release);
        TIMER_HANDLER_SATP.store(0, Ordering::Release);
        TIMER_HANDLER_SP.store(0, Ordering::Release);
        arch::TimerImpl::register_timer_handler(isolated_timer_handler);
        // SAFETY: fixture ensured.
        unsafe { ctl_set(CTL_FLAG, 0) };
        let deadline = arch::TimerImpl::now().saturating_add(1_000_000);
        arch::TimerImpl::set_deadline(deadline);

        let before = read_satp();
        let outcome = isolated::enter(transition);
        let after = read_satp();

        if before != after {
            fail("isolated-timer: Core satp not restored");
        }
        match outcome {
            Outcome::Returned(0x5b) => {}
            _ => fail("isolated-timer: component did not resume to a normal return"),
        }
        if TIMER_COUNT.load(Ordering::Acquire) != 1 {
            fail("isolated-timer: timer not delivered exactly once");
        }
        // 中断必须在 Core root / Core 专用 trap 栈上处理。
        if TIMER_HANDLER_SATP.load(Ordering::Acquire) != before {
            fail("isolated-timer: timer handled outside the Core root");
        }
        let (stack_base, stack_top) = arch::riscv::gateway::core_trap_stack_range();
        let handler_sp = TIMER_HANDLER_SP.load(Ordering::Acquire);
        if handler_sp <= stack_base || handler_sp > stack_top {
            fail("isolated-timer: timer not handled on the Core trap stack");
        }
        // SAFETY: fixture ensured.
        if unsafe { ctl_word(CTL_MAGIC) } != 0x5b5b {
            fail("isolated-timer: component did not write its control page");
        }
        if unsafe { ctl_word(CTL_SATP) } != ISOLATED_INSTANCE_SATP.load(Ordering::Acquire) {
            fail("isolated-timer: component did not observe the private root");
        }
        if unsafe { ctl_word(CTL_ITER) } == 0 {
            fail("isolated-timer: component never looped before the interrupt");
        }
        pass("isolated-timer")
    }

    extern "C" fn isolated_timer_handler() {
        TIMER_COUNT.fetch_add(1, Ordering::AcqRel);
        TIMER_HANDLER_SATP.store(read_satp(), Ordering::Release);
        TIMER_HANDLER_SP.store(read_sp(), Ordering::Release);
        // SAFETY: identity PA view of the instance control page (single CPU).
        unsafe { ctl_set(CTL_FLAG, 1) };
        arch::TimerImpl::cancel_deadline();
    }

    /// fault 用例：组件在私有 AS 里访问未映射页 → gateway trap → Core 策略补映射
    /// → 恢复后重试成功。
    pub(super) fn isolated_fault() -> ! {
        let fixture = match isolated_fixture() {
            Ok(fixture) => fixture,
            Err(reason) => fail(reason),
        };
        // SAFETY: fixture page symbol.
        let entry = isolated_fault_entry as *const () as usize;
        let transition = prepare_or_fail(fixture.handle, entry, false);
        ISOLATED_INSTANCE_SATP.store(transition.satp(), Ordering::Release);
        FAULT_COUNT.store(0, Ordering::Release);
        // 策略只承认"这个地址的 load 缺页"：其他 cause / 地址一律拒绝。
        FAULT_TARGET_VA.store(ISOLATED_DATA_VA, Ordering::Release);
        FAULT_TARGET_PA.store(ISOLATED_DATA_PA.load(Ordering::Acquire), Ordering::Release);
        FAULT_HANDLER_SATP.store(0, Ordering::Release);
        FAULT_HANDLER_SP.store(0, Ordering::Release);
        FAULT_SEPC.store(0, Ordering::Release);
        isolated::install();
        if !isolated::register_fault_policy(isolated_fault_policy) {
            fail("isolated-fault: fault policy registration failed");
        }
        match address_space::translate(fixture.handle, ISOLATED_DATA_VA) {
            Ok(None) => {}
            _ => fail("isolated-fault: data page must start unmapped"),
        }

        let before = read_satp();
        let outcome = isolated::enter(transition);
        let after = read_satp();

        if before != after {
            fail("isolated-fault: Core satp not restored");
        }
        if outcome != Outcome::Returned(0x5c) {
            fail("isolated-fault: component did not recover and return");
        }
        if FAULT_COUNT.load(Ordering::Acquire) != 1 {
            fail("isolated-fault: fault hook did not run exactly once");
        }
        // SAFETY: fixture ensured.
        if unsafe { ctl_word(CTL_DATA) } != DATA_MAGIC {
            fail("isolated-fault: component did not read the recovered page");
        }
        match address_space::translate(fixture.handle, ISOLATED_DATA_VA) {
            Ok(Some(pa)) if pa == ISOLATED_DATA_PA.load(Ordering::Acquire) => {}
            _ => fail("isolated-fault: Core policy did not map the missing page"),
        }
        assert_fault_ran_on_core_context("isolated-fault", before);
        pass("isolated-fault")
    }

    /// abandon 用例：组件跳到未映射地址，Core 策略**拒绝恢复**（组件身份本身
    /// 不是可恢复证明），gateway 放弃组件并回到挂起的 Core 调用者。
    pub(super) fn isolated_fault_abandon() -> ! {
        let fixture = match isolated_fixture() {
            Ok(fixture) => fixture,
            Err(reason) => fail(reason),
        };
        // SAFETY: fixture page symbol.
        let entry = isolated_abandon_entry as *const () as usize;
        let transition = prepare_or_fail(fixture.handle, entry, false);
        ISOLATED_INSTANCE_SATP.store(transition.satp(), Ordering::Release);
        FAULT_COUNT.store(0, Ordering::Release);
        // 同一个窄策略：故障地址不是它承认的那一页 → 拒绝恢复（组件身份本身
        // 不是可恢复的证明）。
        FAULT_TARGET_VA.store(ISOLATED_DATA_VA, Ordering::Release);
        FAULT_TARGET_PA.store(0, Ordering::Release);
        FAULT_HANDLER_SATP.store(0, Ordering::Release);
        FAULT_HANDLER_SP.store(0, Ordering::Release);
        FAULT_SEPC.store(0, Ordering::Release);
        isolated::install();
        if !isolated::register_fault_policy(isolated_fault_policy) {
            fail("isolated-fault-abandon: fault policy registration failed");
        }

        let before = read_satp();
        let outcome = isolated::enter(transition);
        let after = read_satp();

        if before != after {
            fail("isolated-fault-abandon: Core satp not restored");
        }
        if outcome != Outcome::Faulted {
            fail("isolated-fault-abandon: Core must abandon, not resume, the component");
        }
        if FAULT_COUNT.load(Ordering::Acquire) != 1 {
            fail("isolated-fault-abandon: fault hook did not run exactly once");
        }
        if FAULT_SEPC.load(Ordering::Acquire) != ISOLATED_ABANDON_VA {
            fail("isolated-fault-abandon: fault was not attributed to the component context");
        }
        assert_fault_ran_on_core_context("isolated-fault-abandon", before);
        pass("isolated-fault-abandon")
    }

    fn isolated_fault_policy(fault: &mut ComponentFault<'_>) -> FaultDecision {
        FAULT_COUNT.fetch_add(1, Ordering::AcqRel);
        FAULT_HANDLER_SATP.store(read_satp(), Ordering::Release);
        FAULT_HANDLER_SP.store(read_sp(), Ordering::Release);
        FAULT_SEPC.store(fault.frame.epc, Ordering::Release);
        if fault.cause != 13 || fault.stval != FAULT_TARGET_VA.load(Ordering::Acquire) {
            return FaultDecision::Abandon;
        }
        let handle = AddressSpaceHandle::from_raw(
            ISOLATED_HANDLE_ID.load(Ordering::Acquire) as u32,
            ISOLATED_HANDLE_GENERATION.load(Ordering::Acquire) as u32,
        );
        let mapping = Mapping {
            virtual_range: VirtualRange {
                base: fault.stval,
                size: 4096,
            },
            physical_range: PhysicalRange {
                base: FAULT_TARGET_PA.load(Ordering::Acquire),
                size: 4096,
            },
            permission: MappingPermission::READ | MappingPermission::WRITE,
        };
        match address_space::map(handle, mapping) {
            Ok(()) => FaultDecision::Resume,
            Err(_) => FaultDecision::Abandon,
        }
    }

    /// 故障分派必须在 Core root / Core 专用 trap 栈上发生。
    fn assert_fault_ran_on_core_context(case: &str, core_satp: usize) {
        if FAULT_HANDLER_SATP.load(Ordering::Acquire) != core_satp {
            kernel::log!("selftest", "{}: fault handled outside the Core root", case);
            fail("isolated fault handled outside the Core root");
        }
        let (stack_base, stack_top) = arch::riscv::gateway::core_trap_stack_range();
        let handler_sp = FAULT_HANDLER_SP.load(Ordering::Acquire);
        if handler_sp <= stack_base || handler_sp > stack_top {
            kernel::log!("selftest", "{}: fault not on the Core trap stack", case);
            fail("isolated fault not handled on the Core trap stack");
        }
    }

    // -----------------------------------------------------------------------
    // increment 4：真实 `.kcomp` 的按域装载 + 页级权限强制。
    //
    // 夹具 `kcomp_isolated` 零依赖 / 零 import；本模块把它的字节从内嵌 kpkg 读出，
    // 走 `component::isolated_load` 放进一个只含「该镜像各段 + gateway 两页 +
    // 实例栈 + 控制页」的私有 AS，再经 increment 3 的 gateway 进入。**生命
    // 周期仍未接线**：没有任何组件创建路径调用这条链。
    // -----------------------------------------------------------------------

    /// `kcomp_isolated` 的控制页协议槽号（与组件源码逐槽一致）。
    const IMAGE_CTL_COMMAND: usize = 2;
    const IMAGE_CTL_STATUS: usize = 3;
    const IMAGE_CTL_TEXT_VA: usize = 4;
    const IMAGE_CTL_DATA_VA: usize = 5;
    const IMAGE_CTL_RODATA_VA: usize = 6;
    const IMAGE_CTL_RODATA_VALUE: usize = 7;
    const IMAGE_CTL_DATA_VALUE: usize = 8;
    const IMAGE_CTL_BSS_VALUE: usize = 9;
    const IMAGE_CTL_TARGET_VA: usize = 10;
    /// 期望的实例 satp（Core 写；组件用它做环境门禁——见组件源码）。
    const IMAGE_CTL_EXPECT_SATP: usize = 11;

    const IMAGE_MAGIC: usize = 0x4953_4f4c; // "ISOL"
    const IMAGE_RODATA_MAGIC: usize = 0x524f_4441; // "RODA"
    /// 组件 `report()` 写进 data 段再读回的值（`DATA_CELL ^ 0x5555`）。
    const IMAGE_DATA_STORED: usize = 0x4441_5441 ^ 0x5555;
    const IMAGE_BSS_STORED: usize = 0x4242_5353;
    const IMAGE_CANARY_MAGIC: usize = 0x4341_4e41; // "CANA"

    const CMD_REPORT: usize = 0;
    const CMD_STORE_TEXT: usize = 1;
    const CMD_FETCH_DATA: usize = 2;
    const CMD_LOAD_TARGET: usize = 3;

    const R_X: MappingPermission = MappingPermission::READ.union(MappingPermission::EXECUTE);
    const R_W: MappingPermission = MappingPermission::READ.union(MappingPermission::WRITE);

    static IMAGE_FAULT_CAUSE: AtomicUsize = AtomicUsize::new(0);
    static IMAGE_FAULT_STVAL: AtomicUsize = AtomicUsize::new(0);
    static IMAGE_FAULT_PAGE_PA: AtomicUsize = AtomicUsize::new(0);

    /// increment 4 夹具：真实 `.kcomp` 已落进实例 AS + 控制页 + 实例栈 + 一个
    /// **Core 专属**金丝雀页（故意不映射进实例 AS）。
    struct IsolatedImageFixture {
        handle: AddressSpaceHandle,
        image: PlacedImage,
        ctl_pa: usize,
        canary_pa: usize,
    }

    fn map_instance_page(
        handle: AddressSpaceHandle,
        va: usize,
        pa: usize,
        permission: MappingPermission,
    ) -> Result<(), &'static str> {
        address_space::map(
            handle,
            Mapping {
                virtual_range: VirtualRange {
                    base: va,
                    size: 4096,
                },
                physical_range: PhysicalRange {
                    base: pa,
                    size: 4096,
                },
                permission,
            },
        )
        .map_err(|_| "isolated: instance mapping failed")
    }

    fn isolated_image_fixture() -> Result<IsolatedImageFixture, &'static str> {
        if !address_space::isolation_capable() {
            return Err("isolated-image: this profile has no private address space backend");
        }
        let handle = address_space::create_address_space_for(ComponentId::from_raw(0x160))
            .map_err(|_| "isolated-image: create_address_space_for failed")?;

        // 真实 `.kcomp`：字节来自内嵌 kpkg（store 已由 boot 挂载），按域放段。
        let image = match isolated_load::place_artifact(b"kcomp_isolated") {
            Ok(image) => image,
            Err(error) => {
                kernel::log!("selftest", "isolated-image: place failed: {:?}", error);
                return Err("isolated-image: place failed");
            }
        };
        if let Err(error) = isolated_load::map_into(handle, &image) {
            kernel::log!("selftest", "isolated-image: map_into failed: {:?}", error);
            return Err("isolated-image: map_into failed");
        }

        let ctl_pa =
            kernel::memory::vm_page_alloc().map_err(|_| "isolated-image: ctl page alloc")?;
        let stack_pa =
            kernel::memory::vm_page_alloc().map_err(|_| "isolated-image: stack page alloc")?;
        let canary_pa =
            kernel::memory::vm_page_alloc().map_err(|_| "isolated-image: canary page alloc")?;
        map_instance_page(
            handle,
            ISOLATED_CTL_VA,
            ctl_pa,
            MappingPermission::READ | MappingPermission::WRITE,
        )?;
        map_instance_page(
            handle,
            ISOLATED_STACK_BASE,
            stack_pa,
            MappingPermission::READ | MappingPermission::WRITE,
        )?;
        // 金丝雀页**不映射**进实例 AS：它就是"Core 专属映射不可达"的探针。

        // SAFETY: 三个页都来自 vm_page_alloc；identity/low-alias 视图可读写。
        unsafe {
            core::ptr::write_bytes(ctl_pa as *mut u8, 0, 4096);
            core::ptr::write_bytes(stack_pa as *mut u8, 0, 4096);
            core::ptr::write_bytes(canary_pa as *mut u8, 0, 4096);
            core::ptr::write_volatile(canary_pa as *mut usize, IMAGE_CANARY_MAGIC);
        }
        ISOLATED_CTL_PA.store(ctl_pa, Ordering::Release);
        ISOLATED_HANDLE_ID.store(handle.raw_id() as usize, Ordering::Release);
        ISOLATED_HANDLE_GENERATION.store(handle.raw_generation() as usize, Ordering::Release);
        Ok(IsolatedImageFixture {
            handle,
            image,
            ctl_pa,
            canary_pa,
        })
    }

    fn image_segment_of(image: &PlacedImage, address: usize) -> Option<PlacedSegment> {
        image.segments().iter().copied().find(|segment| {
            address >= segment.virtual_range.base
                && address < segment.virtual_range.base + segment.virtual_range.size
        })
    }

    fn image_segment_permission(image: &PlacedImage, address: usize) -> Option<MappingPermission> {
        image_segment_of(image, address).map(|segment| segment.permission)
    }

    /// `va` 所在页必须翻译到该段 backing 的对应页（VA→PA 真相）。
    fn image_page_is_backed(handle: AddressSpaceHandle, image: &PlacedImage, va: usize) -> bool {
        let page = va & !4095;
        let Some(segment) = image_segment_of(image, page) else {
            return false;
        };
        let expected =
            image.mapping(&segment).physical_range.base + (page - segment.virtual_range.base);
        matches!(address_space::translate(handle, page), Ok(Some(pa)) if pa == expected)
    }

    fn assert_unmapped(handle: AddressSpaceHandle, va: usize, reason: &str) {
        if !matches!(address_space::translate(handle, va), Ok(None)) {
            fail(reason);
        }
    }

    /// 主用例：真实 `.kcomp` 的代码在私有 AS 里经 gateway 跑完并返回；data /
    /// rodata 在各自映射 VA 上可读；实例 AS 只含该镜像各段 + gateway 机制页 +
    /// 实例栈 / 控制页，Core 专属映射不可达。
    pub(super) fn isolated_image() -> ! {
        let fixture = match isolated_image_fixture() {
            Ok(fixture) => fixture,
            Err(reason) => fail(reason),
        };
        let image = &fixture.image;

        // (1) 规划真相：页对齐 + 三类权限都存在 + 入口在 R+X 段内。
        let mut rx = 0usize;
        let mut ro = 0usize;
        let mut rw = 0usize;
        for segment in image.segments() {
            if segment.virtual_range.base % 4096 != 0 || segment.virtual_range.size % 4096 != 0 {
                fail("isolated-image: segment is not page aligned");
            }
            match segment.permission {
                permission if permission == R_X => rx += 1,
                permission if permission == R_W => rw += 1,
                permission if permission == MappingPermission::READ => ro += 1,
                _ => fail("isolated-image: unexpected segment permission"),
            }
        }
        if rx == 0 || ro == 0 || rw == 0 {
            fail("isolated-image: fixture must expose R+X / R / R+W segments");
        }
        if image_segment_permission(image, image.create()) != Some(R_X)
            || image_segment_permission(image, image.destroy()) != Some(R_X)
        {
            fail("isolated-image: entries are not in an executable segment");
        }

        // (2) 实例 AS 真相（进入前）：每段与计划逐位一致、页翻译到 backing PA。
        for segment in image.segments() {
            match address_space::mapping_exact(fixture.handle, &segment.virtual_range) {
                Ok(Some(mapping)) if mapping == image.mapping(segment) => {}
                _ => fail("isolated-image: segment mapping does not match the plan"),
            }
            if !image_page_is_backed(fixture.handle, image, segment.virtual_range.base) {
                fail("isolated-image: segment VA is not backed by the planned PA");
            }
        }
        if !matches!(address_space::translate(fixture.handle, ISOLATED_CTL_VA), Ok(Some(pa)) if pa == fixture.ctl_pa)
        {
            fail("isolated-image: control page is not the harness mapping");
        }
        // Core 专属映射：Core 镜像静态 / Core 专用 trap 栈 / 金丝雀页 / 窗口外
        // 地址都必须在实例 AS 里不可达。
        assert_unmapped(
            fixture.handle,
            core::ptr::addr_of!(super::MAPPING_VALUE) as usize,
            "isolated-image: a Core image static is reachable from the instance AS",
        );
        assert_unmapped(
            fixture.handle,
            arch::riscv::gateway::core_trap_stack_range().0,
            "isolated-image: the Core trap stack is reachable from the instance AS",
        );
        assert_unmapped(
            fixture.handle,
            fixture.canary_pa,
            "isolated-image: a Core-only heap page is reachable from the instance AS",
        );
        assert_unmapped(
            fixture.handle,
            image.base() + image.text_size(),
            "isolated-image: memory past the image is reachable from the instance AS",
        );

        // (3) 经 gateway 在私有 AS 里运行真实组件入口。
        unsafe { ctl_set(IMAGE_CTL_COMMAND, CMD_REPORT) };
        let transition = prepare_or_fail(fixture.handle, image.create(), false);
        // gateway 两张机制页必须按同 VA → 同 PA 落成实例侧映射（准备成功即保证，
        // 这里显式钉住"实例 AS = gateway + 镜像段 + harness 页"的真相）。
        for page in arch::riscv::gateway::pages() {
            match address_space::mapping_exact(fixture.handle, &page.virtual_range) {
                Ok(Some(mapping))
                    if mapping.virtual_range == page.virtual_range
                        && mapping.physical_range == page.physical_range
                        && mapping.permission == page.permission => {}
                _ => fail("isolated-image: gateway page mapping missing or mismatched"),
            }
        }
        ISOLATED_INSTANCE_SATP.store(transition.satp(), Ordering::Release);
        let core_satp = read_satp();
        // 组件用它做环境门禁：只在这次 prepare 的私有 AS 里工作。
        unsafe { ctl_set(IMAGE_CTL_EXPECT_SATP, transition.satp()) };
        let outcome = isolated::enter(transition);
        if read_satp() != core_satp {
            fail("isolated-image: Core satp not restored");
        }
        if outcome != Outcome::Returned(0) {
            fail("isolated-image: component did not return 0");
        }

        // (4) 组件证据（只能经实例映射写进控制页）。
        if unsafe { ctl_word(CTL_MAGIC) } != IMAGE_MAGIC {
            fail("isolated-image: component did not write its control page");
        }
        let instance_satp = ISOLATED_INSTANCE_SATP.load(Ordering::Acquire);
        if instance_satp == core_satp {
            fail("isolated-image: private root equals the Core root");
        }
        if unsafe { ctl_word(CTL_SATP) } != instance_satp {
            fail("isolated-image: component did not observe the private root");
        }
        if unsafe { ctl_word(IMAGE_CTL_STATUS) } != 0 {
            fail("isolated-image: component reported a non-zero status");
        }
        let text_va = unsafe { ctl_word(IMAGE_CTL_TEXT_VA) };
        let data_va = unsafe { ctl_word(IMAGE_CTL_DATA_VA) };
        let rodata_va = unsafe { ctl_word(IMAGE_CTL_RODATA_VA) };
        if text_va != image.create() {
            fail("isolated-image: component text VA != planned entry");
        }
        if image_segment_permission(image, text_va) != Some(R_X) {
            fail("isolated-image: text VA is not in an R+X segment");
        }
        if image_segment_permission(image, rodata_va) != Some(MappingPermission::READ) {
            fail("isolated-image: rodata VA is not in a read-only segment");
        }
        if image_segment_permission(image, data_va) != Some(R_W) {
            fail("isolated-image: data VA is not in an R+W segment");
        }

        // (5) 段内容可读（组件在私有 AS 里读到的值）+ VA→PA 交叉验证。
        if unsafe { ctl_word(IMAGE_CTL_RODATA_VALUE) } != IMAGE_RODATA_MAGIC {
            fail("isolated-image: component read unexpected rodata");
        }
        if unsafe { ctl_word(IMAGE_CTL_DATA_VALUE) } != IMAGE_DATA_STORED {
            fail("isolated-image: component data write/read-back failed");
        }
        if unsafe { ctl_word(IMAGE_CTL_BSS_VALUE) } != IMAGE_BSS_STORED {
            fail("isolated-image: component bss write/read-back failed");
        }
        let Some(data_segment) = image_segment_of(image, data_va) else {
            fail("isolated-image: data VA is outside every segment");
        };
        let data_pa = image.mapping(&data_segment).physical_range.base
            + (data_va - data_segment.virtual_range.base);
        // SAFETY: identity/low-alias view of the image backing page.
        if unsafe { core::ptr::read_volatile(data_pa as *const usize) } != IMAGE_DATA_STORED {
            fail("isolated-image: component write is not visible at the backing PA");
        }
        let Some(rodata_segment) = image_segment_of(image, rodata_va) else {
            fail("isolated-image: rodata VA is outside every segment");
        };
        let rodata_pa = image.mapping(&rodata_segment).physical_range.base
            + (rodata_va - rodata_segment.virtual_range.base);
        // SAFETY: identity/low-alias view of the image backing page.
        if unsafe { core::ptr::read_volatile(rodata_pa as *const usize) } != IMAGE_RODATA_MAGIC {
            fail("isolated-image: rodata is not readable at its backing PA");
        }

        // (6) 金丝雀页在组件运行后仍然只属于 Core。
        if unsafe { core::ptr::read_volatile(fixture.canary_pa as *const usize) }
            != IMAGE_CANARY_MAGIC
        {
            fail("isolated-image: Core-only page was modified");
        }
        assert_unmapped(
            fixture.handle,
            fixture.canary_pa,
            "isolated-image: Core-only page became visible after the run",
        );

        kernel::log!(
            "selftest",
            "isolated-image: private AS OK: segments={} rx={} ro={} rw={}",
            image.segments().len(),
            rx,
            ro,
            rw
        );
        pass("isolated-image")
    }

    /// 环境门禁的负向证明：把同一份 `.kcomp` 经 **KernelNative** 生命周期加载
    /// （模拟 `monitor load kcomp_isolated` 这类误用——控制页 VA 在 Core AS 里
    /// 是设备 MMIO），组件必须拒绝（`-EPERM`）且**不写任何槽位**。
    ///
    /// 这条用例**不**走按域装载 / gateway：它证明夹具只在 ArchTest prepare 过的
    /// 私有 AS 里有副作用。
    pub(super) fn isolated_image_wrong_env() -> ! {
        use kernel::component::endpoint::ExecutionDomain;
        use kernel::component::load::{self, ComponentLoadError};
        match load::load_and_start(b"kcomp_isolated", ExecutionDomain::KernelNative) {
            Err(ComponentLoadError::CreateFailed(-1)) => pass("isolated-image-wrong-env"),
            Ok(_) => fail("isolated-image-wrong-env: fixture accepted a KernelNative load"),
            Err(_) => fail("isolated-image-wrong-env: unexpected load error"),
        }
    }

    /// 一次权限强制用例的公共骨架：进入组件 → fault 由 Core 策略观察 → 返回
    /// `Faulted`。返回现场观察值（cause / stval / 故障页翻译结果）。
    ///
    /// `target` 由夹具创建后决定 Core 提供给组件的目标地址（`target_va` 槽）：
    /// 权限用例用 0（组件只用自身上报的地址），`isolated-core-unreachable` 用
    /// 夹具内的 Core 专属金丝雀页。
    struct FaultObservation {
        fixture: IsolatedImageFixture,
        cause: usize,
        stval: usize,
        page_pa: usize,
    }

    fn enter_expecting_fault(
        name: &str,
        command: usize,
        target: fn(&IsolatedImageFixture) -> usize,
    ) -> FaultObservation {
        let fixture = match isolated_image_fixture() {
            Ok(fixture) => fixture,
            Err(reason) => fail(reason),
        };
        let target_va = target(&fixture);
        // 组件先 `report()`（写 text/data VA）再执行命令：命令与 Core 提供的
        // 目标地址在进入前写进控制页。
        unsafe {
            ctl_set(CTL_MAGIC, 0);
            ctl_set(IMAGE_CTL_COMMAND, command);
            ctl_set(IMAGE_CTL_TARGET_VA, target_va);
        }
        let transition = prepare_or_fail(fixture.handle, fixture.image.create(), false);
        FAULT_COUNT.store(0, Ordering::Release);
        IMAGE_FAULT_PAGE_PA.store(0, Ordering::Release);
        isolated::install();
        if !isolated::register_fault_policy(isolated_remember_fault_frame) {
            fail("isolated-perm: fault policy registration failed");
        }
        let core_satp = read_satp();
        // 组件用它做环境门禁：只在这次 prepare 的私有 AS 里工作。
        unsafe { ctl_set(IMAGE_CTL_EXPECT_SATP, transition.satp()) };
        let outcome = isolated::enter(transition);
        if read_satp() != core_satp {
            fail("isolated-perm: Core satp not restored");
        }
        if outcome != Outcome::Faulted {
            fail("isolated-perm: fault was not observed and abandoned");
        }
        if FAULT_COUNT.load(Ordering::Acquire) != 1 {
            fail("isolated-perm: fault hook did not run exactly once");
        }
        assert_fault_ran_on_core_context(name, core_satp);
        FaultObservation {
            fixture,
            cause: IMAGE_FAULT_CAUSE.load(Ordering::Acquire),
            stval: IMAGE_FAULT_STVAL.load(Ordering::Acquire),
            page_pa: IMAGE_FAULT_PAGE_PA.load(Ordering::Acquire),
        }
    }

    /// 只记录现场、拒绝恢复的窄策略：**组件身份本身不是可恢复的证明**；
    /// 断言全部留在 Core（用例）侧。
    fn isolated_remember_fault_frame(fault: &mut ComponentFault<'_>) -> FaultDecision {
        FAULT_COUNT.fetch_add(1, Ordering::AcqRel);
        FAULT_HANDLER_SATP.store(read_satp(), Ordering::Release);
        FAULT_HANDLER_SP.store(read_sp(), Ordering::Release);
        IMAGE_FAULT_CAUSE.store(fault.cause, Ordering::Release);
        IMAGE_FAULT_STVAL.store(fault.stval, Ordering::Release);
        // stval 所在页在 Core ledger 里的翻译结果（0 = 未映射）：它是"页确实
        // 存在，只是权限不允许"与"页根本不存在"的分界证据。
        let handle = AddressSpaceHandle::from_raw(
            ISOLATED_HANDLE_ID.load(Ordering::Acquire) as u32,
            ISOLATED_HANDLE_GENERATION.load(Ordering::Acquire) as u32,
        );
        let page_pa = address_space::translate(handle, fault.stval & !4095)
            .ok()
            .flatten()
            .unwrap_or(0);
        IMAGE_FAULT_PAGE_PA.store(page_pa, Ordering::Release);
        FaultDecision::Abandon
    }

    /// 权限强制的公共断言：故障页在段内、映射到 backing、ledger 权限与计划一致。
    fn assert_permission_fault(
        observation: &FaultObservation,
        expected_cause: usize,
        reported_va: usize,
        expected_permission: MappingPermission,
    ) {
        if observation.cause != expected_cause {
            fail("isolated-perm: unexpected scause");
        }
        if observation.stval != reported_va {
            fail("isolated-perm: stval is not the faulting component address");
        }
        let image = &observation.fixture.image;
        let Some(segment) = image_segment_of(image, observation.stval) else {
            fail("isolated-perm: fault address is outside every image segment");
        };
        if segment.permission != expected_permission {
            fail("isolated-perm: faulting segment has unexpected permission");
        }
        // 页必须真的映射着（否则 fault 只证明"没映射"）。
        let mapping = image.mapping(&segment);
        let expected_pa =
            mapping.physical_range.base + (observation.stval & !4095) - segment.virtual_range.base;
        if observation.page_pa != expected_pa {
            fail("isolated-perm: fault page is not mapped to its backing page");
        }
        match address_space::mapping_exact(observation.fixture.handle, &segment.virtual_range) {
            Ok(Some(recorded)) if recorded == mapping => {}
            _ => fail("isolated-perm: ledger mapping/permission mismatch"),
        }
    }

    /// store 到自己 R+X text 页 → store page fault（scause 0xf）：页表真的拒绝了写。
    pub(super) fn isolated_perm_text() -> ! {
        let observation = enter_expecting_fault("isolated-perm-text", CMD_STORE_TEXT, |_| 0);
        let reported = unsafe { ctl_word(IMAGE_CTL_TEXT_VA) };
        assert_permission_fault(&observation, 15, reported, R_X);
        kernel::log!(
            "selftest",
            "isolated-perm-text: store fault enforced: scause={:#x}, stval={:#x}",
            observation.cause,
            observation.stval
        );
        pass("isolated-perm-text")
    }

    /// instruction fetch 到自己 R+W data 页 → instruction page fault（scause 0xc）。
    pub(super) fn isolated_perm_data() -> ! {
        let observation = enter_expecting_fault("isolated-perm-data", CMD_FETCH_DATA, |_| 0);
        let reported = unsafe { ctl_word(IMAGE_CTL_DATA_VA) };
        assert_permission_fault(&observation, 12, reported, R_W);
        kernel::log!(
            "selftest",
            "isolated-perm-data: fetch fault enforced: scause={:#x}, stval={:#x}",
            observation.cause,
            observation.stval
        );
        pass("isolated-perm-data")
    }

    /// 读 Core 专属页 → load page fault（scause 0xd）：该页在实例 AS 里不可达。
    pub(super) fn isolated_core_unreachable() -> ! {
        let observation =
            enter_expecting_fault("isolated-core-unreachable", CMD_LOAD_TARGET, |f| {
                f.canary_pa
            });
        let canary = observation.fixture.canary_pa;
        if observation.cause != 13 {
            fail("isolated-core-unreachable: expected a load page fault (scause 0xd)");
        }
        if observation.stval != canary {
            fail("isolated-core-unreachable: stval is not the Core-only address");
        }
        if observation.page_pa != 0 {
            fail("isolated-core-unreachable: the Core-only page is mapped in the instance AS");
        }
        // SAFETY: identity/low-alias view of the Core-owned canary page.
        if unsafe { core::ptr::read_volatile(canary as *const usize) } != IMAGE_CANARY_MAGIC {
            fail("isolated-core-unreachable: Core-only page was modified");
        }
        kernel::log!(
            "selftest",
            "isolated-core-unreachable: Core-only page unreachable: scause={:#x}, stval={:#x}",
            observation.cause,
            observation.stval
        );
        pass("isolated-core-unreachable")
    }

    // -----------------------------------------------------------------------
    // increment 5：Isolated 生命周期接线（生产路径 create → Ready → destroy）。
    //
    // 夹具 `kcomp_isolated_life` 经 `load::create_component(..., IsolatedNative)`
    // 创建：私有 AS + 按域镜像 + Core 预置窗口（栈 / 实例窗口）由 Core 建立，
    // `kcomp_instance_create` 经 assembly gateway 在私有 AS 里执行；组件把
    // 观察值写进自己的实例窗口，ArchTest 从 Core 视图读回并断言。destroy 同理。
    // -----------------------------------------------------------------------

    /// `kcomp_isolated_life` 的窗口上报槽号（与组件源码逐槽一致）。
    const LIFE_REPORT_OFF: usize = 512;
    const LIFE_R_MAGIC: usize = 0;
    const LIFE_R_TP: usize = 1;
    const LIFE_R_SATP: usize = 2;
    const LIFE_R_ARGS: usize = 3;
    const LIFE_R_OUT_STATE: usize = 4;
    const LIFE_R_CONFIG_ABI: usize = 5;
    const LIFE_R_CONFIG_LEN: usize = 6;
    const LIFE_R_CONFIG0: usize = 7;
    const LIFE_R_CONFIG1: usize = 8;
    const LIFE_R_SELF: usize = 9;
    /// destroy 标记的**槽号**（= `LIFE_DESTROY_OFF / size_of::<usize>()`）。
    const LIFE_DESTROY_SLOT: usize = 10;
    const LIFE_R_VIEW_KIND: usize = 11;
    const LIFE_R_VIEW_BASE: usize = 12;
    const LIFE_R_VIEW_LEN: usize = 13;

    const LIFE_REPORT_MAGIC: usize = 0x4C49_4645; // "LIFE"
    const LIFE_DESTROY_MAGIC: usize = 0x4C49_4644; // "LIFD"
    /// 成功用例传给组件的 config 负载（Core 必须原样拷进实例窗口）。
    const LIFE_CONFIG: [u8; 2] = [0xC0, 0xDE];
    const LIFE_CONFIG_ABI: u64 = 0x4C49_4645_0001;
    /// 故障注入：create 见到这个 config_abi 立即返回 `-EINVAL`。
    const LIFE_FAIL_ABI: u64 = 0xDEAD_BEEF;
    /// 故障注入：create 见到这个 config_abi 在私有 AS 里执行非法指令。
    const LIFE_FAULT_ABI: u64 = 0xDEAD_FA11;

    /// 从实例窗口 backing 的 Core 视图读一个槽（窗口 PA 由 Core 的映射真相给出）。
    ///
    /// # Safety
    /// `window_pa` 必须是本用例实例窗口 backing 的基址（仍驻留）。
    unsafe fn life_slot(window_pa: usize, index: usize) -> usize {
        unsafe {
            let base = (window_pa + LIFE_REPORT_OFF) as *const usize;
            core::ptr::read_volatile(base.add(index))
        }
    }

    /// 主用例：生产路径创建 → Ready（create 在私有 AS 里跑过）→ 销毁 → Stopped。
    pub(super) fn isolated_lifecycle() -> ! {
        use kernel::component::containment::KcompCreateArgs;
        use kernel::component::endpoint::ExecutionDomain;
        use kernel::component::isolated_lifecycle::{
            self, ISOLATED_STACK_BASE, WINDOW_OUT_STATE_OFF, WINDOW_RUNTIME_OFF,
        };
        use kernel::component::load;
        use kernel::component::registry;
        use kernel::component::runtime_slot;
        use kernel::component::ComponentState;
        use kernel::memory::address_space::{self, MapError};

        let core_satp = read_satp();
        let args = KcompCreateArgs {
            config_abi: LIFE_CONFIG_ABI,
            config: LIFE_CONFIG.as_ptr() as *const (),
            config_len: LIFE_CONFIG.len(),
        };

        // When：经生产入口创建一个 Isolated 实例。
        let id = match load::create_component(
            b"kcomp_isolated_life",
            &args,
            ExecutionDomain::IsolatedNative,
        ) {
            Ok(id) => id,
            Err(error) => {
                kernel::log!("selftest", "isolated-lifecycle: create failed: {:?}", error);
                fail("isolated-lifecycle: create failed");
            }
        };

        // Then：Core AS 已恢复，实例 Ready，AS 句柄 / state / 镜像都在 Core 真相里。
        if read_satp() != core_satp {
            fail("isolated-lifecycle: Core satp not restored after create");
        }
        let (state, handle, instance_state) = {
            let reg = registry::get_registry().lock();
            let record = match reg.get(id) {
                Some(record) => record,
                None => fail("isolated-lifecycle: instance record missing"),
            };
            (
                record.state,
                record.address_space,
                record.instance_state as usize,
            )
        };
        if state != ComponentState::Ready {
            fail("isolated-lifecycle: instance did not reach Ready");
        }
        let handle = match handle {
            Some(handle) => handle,
            None => fail("isolated-lifecycle: instance has no address space"),
        };

        // 实例窗口的映射真相 + Core 视图（窗口 backing 由 Core 预置）。
        let window = isolated_lifecycle::window_range();
        let window_pa = match address_space::mapping_exact(handle, &window) {
            Ok(Some(mapping)) => mapping.physical_range.base,
            _ => fail("isolated-lifecycle: instance window is not mapped"),
        };

        // (a) 组件写回的 out_state 是**实例内 VA**，指向窗口里的上报区。
        let expected_report = window.base + LIFE_REPORT_OFF;
        if instance_state != expected_report {
            fail("isolated-lifecycle: out_state is not the in-window report address");
        }

        // (b) 上报内容：组件真的在私有 AS 里跑过、args / config / tp 都对得上。
        // SAFETY: 窗口 backing 由 Core 分配且仍驻留；索引都在上报页内。
        let slot = |index: usize| unsafe { life_slot(window_pa, index) };
        if slot(LIFE_R_MAGIC) != LIFE_REPORT_MAGIC {
            fail("isolated-lifecycle: component did not write its window report");
        }
        let expected_satp = match address_space::prepare_activation(handle) {
            Ok(activation) => activation.token().satp(),
            Err(_) => fail("isolated-lifecycle: prepare_activation failed"),
        };
        if slot(LIFE_R_SATP) != expected_satp {
            fail("isolated-lifecycle: component did not observe the private root");
        }
        if expected_satp == core_satp {
            fail("isolated-lifecycle: private root equals the Core root");
        }
        if slot(LIFE_R_ARGS) != window.base {
            fail("isolated-lifecycle: create args were not delivered in the window");
        }
        if slot(LIFE_R_OUT_STATE) != window.base + WINDOW_OUT_STATE_OFF {
            fail("isolated-lifecycle: component did not see its out_state slot");
        }
        if slot(LIFE_R_SELF) == 0 {
            fail("isolated-lifecycle: component self address is zero");
        }
        if slot(LIFE_R_CONFIG_ABI) != LIFE_CONFIG_ABI as usize
            || slot(LIFE_R_CONFIG_LEN) != LIFE_CONFIG.len()
            || slot(LIFE_R_CONFIG0) != LIFE_CONFIG[0] as usize
            || slot(LIFE_R_CONFIG1) != LIFE_CONFIG[1] as usize
        {
            fail("isolated-lifecycle: config payload was not delivered");
        }
        // (b2) Core 预交付的域视图：kind = LOCAL_VA、base/len = 本实例窗口
        //      （表示是实例内 VA，绝不是物理地址 / Core 私有 VA）。
        use kernel::generated::abi::KCORE_MEMORY_VIEW_LOCAL_VA;
        if slot(LIFE_R_VIEW_KIND) != KCORE_MEMORY_VIEW_LOCAL_VA as usize {
            fail("isolated-lifecycle: window view is not LOCAL_VA");
        }
        if slot(LIFE_R_VIEW_BASE) != window.base || slot(LIFE_R_VIEW_LEN) != window.size {
            fail("isolated-lifecycle: window view does not describe the instance window");
        }
        // (c) runtime context：`tp` 就是 Core 为该实例安装的实例内 slot。
        if slot(LIFE_R_TP) != window.base + WINDOW_RUNTIME_OFF {
            fail("isolated-lifecycle: per-instance runtime slot (tp) not installed");
        }
        if runtime_slot::get_slots().lock().get(id) as usize != window.base + WINDOW_RUNTIME_OFF {
            fail("isolated-lifecycle: runtime slot table disagrees with the window");
        }

        // (d) 窗口只属于本实例：另一个 AS 不映射这个 VA，窗口 VA 也不在 Core 的
        //     恒等映射 RAM 窗口里（Core AS 看不到它）。
        let other = match address_space::create_address_space_for(ComponentId::from_raw(0x1A5E)) {
            Ok(other) => other,
            Err(_) => fail("isolated-lifecycle: second address space creation failed"),
        };
        if !matches!(address_space::translate(other, window.base), Ok(None)) {
            fail("isolated-lifecycle: instance window is reachable from another AS");
        }
        if !matches!(
            address_space::translate(other, ISOLATED_STACK_BASE),
            Ok(None)
        ) {
            fail("isolated-lifecycle: component stack is reachable from another AS");
        }
        let _ = address_space::retire(other);
        if window.base >= 0x8000_0000 {
            fail("isolated-lifecycle: window VA overlaps the Core RAM identity window");
        }

        // When：优雅停止（生产路径）。
        if let Err(error) = kernel::component::stop_component(id) {
            kernel::log!("selftest", "isolated-lifecycle: stop failed: {:?}", error);
            fail("isolated-lifecycle: stop failed");
        }

        // Then：Core AS 恢复、Stopped、destroy 入口真的执行过、AS 已退役。
        if read_satp() != core_satp {
            fail("isolated-lifecycle: Core satp not restored after destroy");
        }
        if registry::get_registry().lock().get(id).map(|r| r.state) != Some(ComponentState::Stopped)
        {
            fail("isolated-lifecycle: instance did not reach Stopped");
        }
        // SAFETY: 窗口 backing 在销毁后仍驻留（phase 1 逻辑死亡 / 物理驻留）。
        if slot(LIFE_DESTROY_SLOT) != LIFE_DESTROY_MAGIC {
            fail("isolated-lifecycle: destroy entry did not run");
        }
        match address_space::prepare_activation(handle) {
            Err(MapError::Retired) => {}
            _ => fail("isolated-lifecycle: address space was not retired"),
        }
        kernel::log!(
            "selftest",
            "isolated-lifecycle: private AS OK: id={}, window={:#x}, satp={:#x}",
            id.raw(),
            window.base,
            expected_satp
        );
        pass("isolated-lifecycle")
    }

    /// create 失败的公共终态断言：实例留 tombstone（`Failed`）、AS 退役、
    /// Core 预置窗口 / 栈归还、runtime slot 清空（半成品不留）。
    fn assert_failed_isolated_cleanup() {
        use kernel::component::endpoint::ExecutionDomain;
        use kernel::component::isolated_lifecycle;
        use kernel::component::registry;
        use kernel::component::runtime_slot;
        use kernel::component::ComponentState;
        use kernel::memory::address_space::{self, MapError};

        let (id, handle) = {
            let reg = registry::get_registry().lock();
            let mut found = None;
            for record in reg.iter() {
                if record.execution_domain == ExecutionDomain::IsolatedNative
                    && record.state == ComponentState::Failed
                {
                    found = Some((record.id, record.address_space));
                }
            }
            match found {
                Some((id, Some(handle))) => (id, handle),
                _ => fail("isolated create failure: no failed Isolated instance with an AS"),
            }
        };
        match address_space::prepare_activation(handle) {
            Err(MapError::Retired) => {}
            _ => fail("isolated create failure: address space was not retired"),
        }
        if !matches!(
            address_space::mapping_exact(handle, &isolated_lifecycle::window_range()),
            Ok(None)
        ) {
            fail("isolated create failure: instance window mapping leaked");
        }
        if !matches!(
            address_space::mapping_exact(handle, &isolated_lifecycle::stack_range()),
            Ok(None)
        ) {
            fail("isolated create failure: component stack mapping leaked");
        }
        if !runtime_slot::get_slots().lock().get(id).is_null() {
            fail("isolated create failure: runtime slot was not cleared");
        }
    }

    /// 失败路径（create 返回非零）：Failed + AS 退役 + Core 预置窗口归还，
    /// 不留半成品实例。
    pub(super) fn isolated_lifecycle_fail() -> ! {
        use kernel::component::containment::KcompCreateArgs;
        use kernel::component::endpoint::ExecutionDomain;
        use kernel::component::load::{self, ComponentLoadError};

        let core_satp = read_satp();
        let args = KcompCreateArgs {
            config_abi: LIFE_FAIL_ABI,
            config: core::ptr::null(),
            config_len: 0,
        };

        // When：create 在组件入口里失败（-EINVAL）。
        let error = match load::create_component(
            b"kcomp_isolated_life",
            &args,
            ExecutionDomain::IsolatedNative,
        ) {
            Err(error) => error,
            Ok(_) => fail("isolated-lifecycle-fail: create succeeded with the fail config"),
        };
        if error != ComponentLoadError::CreateFailed(-22) {
            kernel::log!(
                "selftest",
                "isolated-lifecycle-fail: unexpected create error: {:?}",
                error
            );
            fail("isolated-lifecycle-fail: unexpected create error");
        }
        if read_satp() != core_satp {
            fail("isolated-lifecycle-fail: Core satp not restored");
        }

        // Then：tombstone + 清理（公共断言）。
        assert_failed_isolated_cleanup();
        pass("isolated-lifecycle-fail")
    }

    /// 失败路径（create 在私有 AS 里故障）：gateway 故障分派判不可恢复
    /// （`Outcome::Faulted`）→ `CreateFaulted` + 同一套清理，绝不把 Core 打 panic。
    pub(super) fn isolated_lifecycle_fault() -> ! {
        use kernel::component::containment::KcompCreateArgs;
        use kernel::component::endpoint::ExecutionDomain;
        use kernel::component::load::{self, ComponentLoadError};

        let core_satp = read_satp();
        let args = KcompCreateArgs {
            config_abi: LIFE_FAULT_ABI,
            config: core::ptr::null(),
            config_len: 0,
        };

        // When：create 在私有 AS 里执行非法指令。
        let error = match load::create_component(
            b"kcomp_isolated_life",
            &args,
            ExecutionDomain::IsolatedNative,
        ) {
            Err(error) => error,
            Ok(_) => fail("isolated-lifecycle-fault: create succeeded with the fault config"),
        };
        if error != ComponentLoadError::CreateFaulted {
            kernel::log!(
                "selftest",
                "isolated-lifecycle-fault: unexpected create error: {:?}",
                error
            );
            fail("isolated-lifecycle-fault: unexpected create error");
        }
        if read_satp() != core_satp {
            fail("isolated-lifecycle-fault: Core satp not restored after the fault");
        }

        // Then：tombstone + 清理（与返回非零同一路径）。
        assert_failed_isolated_cleanup();
        pass("isolated-lifecycle-fault")
    }
}

fn context_switch() -> ! {
    // SAFETY: this single-hart test initializes every static context before use.
    unsafe {
        core::ptr::addr_of_mut!(SELFTEST_CONTEXT_A).write(MaybeUninit::new(
            arch::CpuImpl::new_context(
                selftest_context_a_entry as *const () as usize,
                stack_a_top(),
            ),
        ));
        core::ptr::addr_of_mut!(SELFTEST_CONTEXT_B).write(MaybeUninit::new(
            arch::CpuImpl::new_context(
                selftest_context_b_entry as *const () as usize,
                stack_b_top(),
            ),
        ));
        core::ptr::addr_of_mut!(SELFTEST_RETURN_CONTEXT)
            .write(MaybeUninit::new(arch::CpuImpl::new_context(0, 0)));
        arch::CpuImpl::context_switch(return_context(), context_a());
    }
    fail("context switch returned to bootstrap")
}

#[unsafe(no_mangle)]
extern "C" fn selftest_context_a_resumed() -> ! {
    if !registers_match(core::ptr::addr_of!(SELFTEST_A_S), 0x11) {
        fail("context A s-registers changed");
    }
    if !stack_matches(true) {
        fail("context A stack pointer changed");
    }
    // SAFETY: assembly saved A before transferring here; resuming B verifies it.
    unsafe { arch::CpuImpl::context_switch(context_a(), context_b()) }
    fail("context A unexpectedly resumed")
}

#[unsafe(no_mangle)]
extern "C" fn selftest_context_b_resumed() -> ! {
    if !registers_match(core::ptr::addr_of!(SELFTEST_B_S), 0x21) {
        fail("context B s-registers changed");
    }
    if !stack_matches(false) {
        fail("context B stack pointer changed");
    }
    pass("context-switch")
}

fn registers_match(values: *const [usize; 12], first: usize) -> bool {
    // SAFETY: assembly filled every array slot before tail-calling the checker.
    let values = unsafe { &*values };
    values
        .iter()
        .enumerate()
        .all(|(index, value)| *value == first + index)
}

fn stack_matches(a: bool) -> bool {
    let (expected, actual) = if a {
        (
            core::ptr::addr_of!(SELFTEST_A_SP),
            core::ptr::addr_of!(SELFTEST_A_RESUMED_SP),
        )
    } else {
        (
            core::ptr::addr_of!(SELFTEST_B_SP),
            core::ptr::addr_of!(SELFTEST_B_RESUMED_SP),
        )
    };
    // SAFETY: assembly captured both values before Rust executes in this context.
    unsafe { expected.read() == actual.read() }
}

fn stack_a_top() -> usize {
    stack_top(core::ptr::addr_of_mut!(STACK_A))
}

fn stack_b_top() -> usize {
    stack_top(core::ptr::addr_of_mut!(STACK_B))
}

fn stack_top(stack: *mut Stack) -> usize {
    // SAFETY: this only forms the one-past-end address of a static stack.
    unsafe {
        core::ptr::addr_of_mut!((*stack).0)
            .cast::<u8>()
            .add(STACK_BYTES) as usize
    }
}

unsafe fn context_a() -> &'static mut arch::ContextImpl {
    // SAFETY: initialized exactly once before the first context switch.
    unsafe { &mut *core::ptr::addr_of_mut!(SELFTEST_CONTEXT_A).cast::<arch::ContextImpl>() }
}

unsafe fn context_b() -> &'static mut arch::ContextImpl {
    // SAFETY: initialized exactly once before the first context switch.
    unsafe { &mut *core::ptr::addr_of_mut!(SELFTEST_CONTEXT_B).cast::<arch::ContextImpl>() }
}

unsafe fn return_context() -> &'static mut arch::ContextImpl {
    // SAFETY: initialized exactly once before the first context switch.
    unsafe { &mut *core::ptr::addr_of_mut!(SELFTEST_RETURN_CONTEXT).cast::<arch::ContextImpl>() }
}

#[cfg(target_arch = "riscv32")]
unsafe fn rv32_unmap(address: usize) {
    let root = unsafe { rv32_root() };
    unsafe { root.add(address >> 22).write_volatile(0) };
    unsafe { core::arch::asm!("sfence.vma", options(nostack, preserves_flags)) };
}

#[cfg(target_arch = "riscv32")]
unsafe fn rv32_set_page_permissions(address: usize, flags: u32) {
    let page_base = address & !0x003f_ffff;
    let table = unsafe { &mut *core::ptr::addr_of_mut!(SV32_TEST_TABLE) };
    for (index, entry) in table.iter_mut().enumerate() {
        *entry = (((page_base + index * 4096) as u32 >> 12) << 10) | 0xcf;
    }
    table[(address >> 12) & 0x3ff] = ((address as u32 >> 12) << 10) | flags;
    let root = unsafe { rv32_root() };
    let table_pte = (((table.as_ptr() as usize >> 12) as u32) << 10) | 1;
    unsafe { root.add(address >> 22).write_volatile(table_pte) };
    unsafe { core::arch::asm!("sfence.vma", options(nostack, preserves_flags)) };
}

#[cfg(target_arch = "riscv32")]
unsafe fn rv32_root() -> *mut u32 {
    let satp: usize;
    unsafe {
        core::arch::asm!("csrr {satp}, satp", satp = out(reg) satp, options(nostack, preserves_flags));
    }
    ((satp & 0x003f_ffff) << 12) as *mut u32
}

/// TLB 用例后备页的物理地址（`#[repr(align(4096))]`，页对齐）。
///
/// # Safety
/// `page` 必须指向一个活的测试页。
unsafe fn test_page_pa(page: *mut TestPage) -> usize {
    // SAFETY: 调用者保证指针有效；只取首字节地址，不解引用内容。
    arch::physical_address_of(unsafe { (*page).0.as_mut_ptr() } as usize)
}

/// 在**活动** satp 页表里重写 `va` 的 4 KiB 叶子（测试侧 poke）。
///
/// 只服务 TLB 用例：`va` 是测试空洞，调用方用 `flags` 指定叶子权限，
/// `flags = 0` 表示撤销。中间层在 RV64 上优先复用现有表；写完必 `sfence.vma`。
///
/// # Safety
/// `va` / `pa` 必须页对齐，且 `va` 除 TLB 用例之外没有别的使用者。
unsafe fn selftest_map_page(va: usize, pa: usize, flags: usize) {
    #[cfg(target_arch = "riscv32")]
    {
        // SAFETY: TLB_TABLE32 是静态测试表，页对齐且只被本用例使用。
        let entries = unsafe { &mut (*core::ptr::addr_of_mut!(TLB_TABLE32)).0 };
        // RV32 全程 identity（VA == PA）：表指针本身即物理地址。
        let link = (((entries.as_ptr() as usize >> 12) as u32) << 10) | PTE_V as u32;
        let root = unsafe { rv32_root() };
        unsafe { root.add(va >> 22).write_volatile(link) };
        entries.fill(0);
        entries[(va >> 12) & 0x3ff] = (((pa >> 12) as u32) << 10) | flags as u32;
    }

    #[cfg(target_arch = "riscv64")]
    {
        let satp: usize;
        unsafe {
            core::arch::asm!("csrr {satp}, satp", satp = out(reg) satp, options(nostack, preserves_flags));
        }
        let root = ((satp & ((1 << 44) - 1)) << 12) as *mut u64;
        // SAFETY: 两个测试表页对齐、只被本用例使用；PA 由恒等/低别名约定给出。
        let l2_entries = unsafe { &mut (*core::ptr::addr_of_mut!(TLB_L2)).0 };
        let l2_pa = arch::physical_address_of(l2_entries.as_mut_ptr() as usize);
        let l2 = unsafe { sv39_child_table(root, (va >> 30) & 0x1ff, l2_pa) };
        let l1_entries = unsafe { &mut (*core::ptr::addr_of_mut!(TLB_L1)).0 };
        let l1_pa = arch::physical_address_of(l1_entries.as_mut_ptr() as usize);
        let l1 = unsafe { sv39_child_table(l2, (va >> 21) & 0x1ff, l1_pa) };
        let leaf = ((pa >> 12) as u64) << 10 | flags as u64;
        unsafe { l1.add((va >> 12) & 0x1ff).write_volatile(leaf) };
    }

    unsafe {
        core::arch::asm!("sfence.vma", options(nostack, preserves_flags));
    }
}

/// RV64：取（必要时安装）`parent[index]` 的下级表——已有表复用，无效或大叶
/// 槽位换成 `fallback_pa`。返回的指针按 identity 约定可直接解引用。
///
/// # Safety
/// `parent` 必须指向活动的 Sv39 页表页，`fallback_pa` 必须是页对齐的测试表。
#[cfg(target_arch = "riscv64")]
unsafe fn sv39_child_table(parent: *mut u64, index: usize, fallback_pa: usize) -> *mut u64 {
    let entry = unsafe { parent.add(index) };
    let bits = unsafe { entry.read_volatile() };
    let is_leaf = bits & ((PTE_R | PTE_W | (1 << 3)) as u64) != 0;
    if bits & PTE_V as u64 != 0 && !is_leaf {
        return (((bits >> 10) & ((1 << 44) - 1)) << 12) as *mut u64;
    }
    unsafe { entry.write_volatile((((fallback_pa >> 12) as u64) << 10) | PTE_V as u64) };
    fallback_pa as *mut u64
}

fn pass(name: &str) -> ! {
    kernel::log!("selftest", "{}: PASS", name);
    arch::ResetImpl::system_reset(ResetType::Shutdown)
}

fn fail(reason: &str) -> ! {
    kernel::log!("selftest", "FAIL: {}", reason);
    panic!("selftest failed: {}", reason)
}
