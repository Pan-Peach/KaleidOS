use arch::{CpuArch, InterruptController, ResetType, SystemReset, Timer};
use core::sync::atomic::{AtomicUsize, Ordering};
use core::{arch::global_asm, mem::MaybeUninit};
use kernel::machine::{CpuId, IoSpace, MachineInfo};

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
        // SMP 用例（rv64 only）：由 arch_runner 的 `--smp` 名单驱动（默认 `test-arch`
        // 不发送这些名字，所以它们恒编译也无害）。
        #[cfg(target_arch = "riscv64")]
        b"smp-boot" => smp_boot(info),
        #[cfg(target_arch = "riscv64")]
        b"smp-ipi" => smp_ipi(info),
        #[cfg(target_arch = "riscv64")]
        b"smp-percpu" => smp_percpu(info),
        // 私有 AS 跨 AS trampoline（机制证明）。
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-transition" => isolated_tests::isolated_transition(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-timer" => isolated_tests::isolated_timer(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-fault" => isolated_tests::isolated_fault(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-fault-abandon" => isolated_tests::isolated_fault_abandon(),
        // 真实 `.kcomp` 的按域装载 + 页级权限强制（直接驱动机制，不经生命周期）。
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-image" => isolated_tests::isolated_image(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-image-wrong-env" => isolated_tests::isolated_image_wrong_env(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-perm-text" => isolated_tests::isolated_perm_text(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-perm-data" => isolated_tests::isolated_perm_data(),
        // 共享 Core 映射模型：same VA→PA、私有 backing 别名排除、Core 直接调用。
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-shared-mappings" => isolated_tests::isolated_shared_mappings(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-private-unreachable" => isolated_tests::isolated_private_unreachable(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-core-direct" => isolated_tests::isolated_core_direct(),
        // Isolated 生命周期（生产 create → Ready → destroy）。
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-lifecycle" => isolated_tests::isolated_lifecycle(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-lifecycle-fail" => isolated_tests::isolated_lifecycle_fail(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-lifecycle-fault" => isolated_tests::isolated_lifecycle_fault(),
        // KernelNative caller → Isolated provider 的跨域 service Gate
        // （caller 帧直接交付 + 跨 AS trampoline；故障 containment）。
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-service" => isolated_tests::isolated_service(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-service-fault" => isolated_tests::isolated_service_fault(),
        // 失败 / 重启矩阵（每个阶段失败的不变量 + stale 访问阻断 +
        // 逻辑重启）。
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-load-reject" => isolated_tests::isolated_load_reject(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-config-reject" => isolated_tests::isolated_config_reject(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-prepare-reject" => isolated_tests::isolated_prepare_reject(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-destroy-fault" => isolated_tests::isolated_destroy_fault(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-stale-access" => isolated_tests::isolated_stale_access(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-ready-fault" => isolated_tests::isolated_ready_fault(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-restart" => isolated_tests::isolated_restart(),
        // 直接 Core import（支持面）+ 组件 panic 的跨 AS 收敛。
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-direct-imports" => isolated_tests::isolated_direct_imports(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-panic-escape" => isolated_tests::isolated_panic_escape(),
        // 嵌套 AS：Core/AS_A → AS_B → AS_A（健康 + B 故障两条路径）。
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-nested-as" => isolated_tests::isolated_nested_as(),
        #[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
        b"isolated-nested-fault" => isolated_tests::isolated_nested_fault(),
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

/// **真实 `.kcomp`** 的 panic containment。
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
        // 先取出 bool，避免块尾表达式把 `reg` 的借用拖过局部变量析构。
        let any = reg.iter().any(|record| {
            record.name.as_slice() == b"kcomp_panic"
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

fn timer_handler(_cpu: CpuId) {
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

    let _ = find_mmio(
        info,
        &[b"riscv,plic0".as_slice(), b"sifive,plic-1.0.0".as_slice()],
    )
    .expect("PLIC device not found");
    let (uart_base, _) = find_mmio(info, &[b"ns16550a".as_slice()]).expect("UART device not found");
    let uart_line = find_irq(info, &[b"ns16550a".as_slice()]).expect("UART irq not found");
    let ier = uart_base + UART_IER_OFFSET;

    // PLIC 已由 boot 全局配置（`configure` 在 UP 阶段只允许一次）；此用例只注册
    // handler 并使能线，不再重复 configure。
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

fn external_irq_handler(_cpu: CpuId) {
    // 顺序要紧：**先 claim 再关源**。claim 读走 pending 并置 in-service、才拿得到 id；
    // 若先关设备（UART 电平触发），pending 随电平撤销，claim 会返回 0。
    if let Some(claim) = arch::InterruptImpl::claim() {
        let line = arch::InterruptImpl::claim_line(&claim);
        EXTERNAL_IRQ_LINE.store(line as usize, Ordering::Release);
        // 关中断源（THRE 电平触发：不关的话 complete 后立刻又 pending = 风暴）
        let ier = UART_IER_ADDR.load(Ordering::Acquire);
        if ier != 0 {
            // SAFETY: 已发现 UART 的 IER 寄存器（字节宽）。
            unsafe { core::ptr::write_volatile(ier as *mut u8, 0) };
        }
        arch::InterruptImpl::complete(claim);
    }
    EXTERNAL_IRQ_COUNT.fetch_add(1, Ordering::AcqRel);
}

/// SMP 用例：启动次 CPU 并证明它进入了 `secondary_main`（milestone 1）。
///
/// 触发 boot 侧的 AP bring-up（`start_secondaries`），然后等 `ONLINE` 计数达到
/// 非 boot CPU 数；超时即失败。`-smp 2` 由 `arch_runner.py --smp` 提供。
#[cfg(target_arch = "riscv64")]
fn smp_boot(info: &MachineInfo) -> ! {
    let want = info.cpu_count.saturating_sub(1); // 非 boot CPU 数
    if want == 0 {
        fail("smp-boot: only one CPU discovered (QEMU needs -smp 2)");
    }
    crate::smp::start_secondaries(info);

    let deadline = arch::TimerImpl::now().saturating_add(10_000_000);
    while crate::smp::ONLINE.load(Ordering::Acquire) < want && arch::TimerImpl::now() < deadline {
        core::hint::spin_loop();
    }
    let online = crate::smp::ONLINE.load(Ordering::Acquire);
    if online < want {
        kernel::log!("selftest", "smp-boot online={} want={}", online, want);
        fail("smp-boot: secondary CPU never reached secondary_main");
    }
    pass("smp-boot")
}

/// SMP 用例：BSP 给每个 AP 发一个 IPI 门铃，证明目标 CPU 真的处理了它。
///
/// 白盒驱动 arch 的 IPI 机制（类似 `external-irq`）：注册全局 handler → 等 AP
/// 完成本地初始化（`Smp::init_cpu` / `enable_ipi_interrupt` / `enable_irq`）→
/// 发门铃 → 等目标 CPU 上的 handler 计数。
#[cfg(target_arch = "riscv64")]
fn smp_ipi(info: &MachineInfo) -> ! {
    use arch::smp::Smp;

    let want = info.cpu_count.saturating_sub(1);
    if want == 0 {
        fail("smp-ipi: only one CPU discovered (QEMU needs -smp 2)");
    }
    crate::smp::start_secondaries(info);

    // BSP 注册全局 IPI handler，并打开自己的 IPI 接收（必须在任何 CPU 开接收之前）。
    <arch::SmpImpl as Smp>::register_ipi_handler(bsp_ipi_handler)
        .expect("register_ipi_handler failed");
    <arch::SmpImpl as Smp>::init_cpu().expect("Smp::init_cpu failed");
    <arch::SmpImpl as Smp>::enable_ipi_interrupt();

    // **先等所有 AP online** 再发门铃：AP 的 `Smp::init_cpu` 会清 `sip.SSIP`，
    // 若门铃早于它到达就会被清掉、永远不投递（真实的启动期竞态）。
    let deadline = arch::TimerImpl::now().saturating_add(10_000_000);
    while crate::smp::ONLINE.load(Ordering::Acquire) < want && arch::TimerImpl::now() < deadline {
        core::hint::spin_loop();
    }
    if crate::smp::ONLINE.load(Ordering::Acquire) < want {
        fail("smp-ipi: AP did not reach secondary_main");
    }

    // 给每个非 boot CPU 发门铃。
    for cpu in &info.cpu_info[..info.cpu_count] {
        if cpu.hardware_id == info.boot_hardware_id {
            continue;
        }
        <arch::SmpImpl as Smp>::send_ipi(cpu.hardware_id).expect("send_ipi failed");
    }

    let deadline = arch::TimerImpl::now().saturating_add(10_000_000);
    while crate::smp::IPI_SEEN.load(Ordering::Acquire) < want && arch::TimerImpl::now() < deadline {
        core::hint::spin_loop();
    }
    if crate::smp::IPI_SEEN.load(Ordering::Acquire) < want {
        fail("smp-ipi: target CPU did not handle the IPI");
    }
    pass("smp-ipi")
}

/// IPI handler：在**目标** CPU 上运行，只计数（证明门铃投递到了非 boot CPU）。
#[cfg(target_arch = "riscv64")]
fn bsp_ipi_handler(cpu: CpuId) {
    if cpu.raw() != 0 {
        crate::smp::IPI_SEEN.fetch_add(1, Ordering::AcqRel);
    }
}

/// SMP 用例：证明 per-CPU 身份彼此独立——每个 AP 读到的 `current_cpu()` 必须
/// 等于它自己的逻辑下标。
///
/// 依赖 `secondary_main` 装自己的入口记录（已实现）。Core 的 per-CPU
/// sched/timer/containment 状态（各 `init_cpu`）是后续一步。
#[cfg(target_arch = "riscv64")]
fn smp_percpu(info: &MachineInfo) -> ! {
    let want = info.cpu_count.saturating_sub(1);
    if want == 0 {
        fail("smp-percpu: only one CPU discovered (QEMU needs -smp 2)");
    }
    crate::smp::start_secondaries(info);

    let deadline = arch::TimerImpl::now().saturating_add(10_000_000);
    while crate::smp::ONLINE.load(Ordering::Acquire) < want && arch::TimerImpl::now() < deadline {
        core::hint::spin_loop();
    }
    if crate::smp::ONLINE.load(Ordering::Acquire) < want {
        fail("smp-percpu: AP did not reach secondary_main");
    }

    for i in 1..info.cpu_count {
        if crate::smp::PERCPU_IDS[i].load(Ordering::Acquire) != i {
            fail("smp-percpu: AP current_cpu() != its logical id");
        }
    }
    if arch::CpuImpl::current_cpu().map(|c| c.raw()) != Some(0) {
        fail("smp-percpu: BSP current_cpu() != 0");
    }
    pass("smp-percpu")
}

// ---------------------------------------------------------------------------
// Isolated 域 ArchTest（见 `selftest/isolated/` 各职责子模块）。
// ---------------------------------------------------------------------------
#[cfg(all(feature = "supervisor", feature = "vm-mmu"))]
#[path = "selftest/isolated/mod.rs"]
mod isolated_tests;

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
