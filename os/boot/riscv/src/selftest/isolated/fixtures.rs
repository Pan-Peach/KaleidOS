//! Isolated ArchTest fixtures: private-AS setup, control page, instance stack,
//! register probes.  Shared by the transition / trap / image test groups.

/// 实例 AS 的 VA 布局（与 `isolated{64,32}.S` 里的常量一致）。
pub(crate) const ISOLATED_CTL_VA: usize = 0x3000_0000;
pub(crate) const ISOLATED_DATA_VA: usize = 0x3000_1000;
pub(crate) const ISOLATED_STACK_BASE: usize = 0x3000_2000;
pub(crate) const ISOLATED_STACK_SIZE: usize = 4096;
/// 夹具代码页在实例 AS 里的**私有** RV 映射（页面 backing 是 Core 镜像页）。
pub(crate) const ISOLATED_FIXTURE_VA: usize = 0x3000_3000;
pub(crate) const ISOLATED_ABANDON_VA: usize = 0x5000_0000;

/// 控制页槽号（字节偏移 = 槽号 × `size_of::<usize>()`）。
pub(crate) const CTL_MAGIC: usize = 0;
pub(crate) const CTL_SATP: usize = 1;
pub(crate) const CTL_FLAG: usize = 2;
pub(crate) const CTL_ITER: usize = 3;
pub(crate) const CTL_DATA: usize = 4;

/// 预置在惰性数据页里的值（fault 恢复后组件读到并写回控制页）。
pub(crate) const DATA_MAGIC: usize = 0x4d41_4749; // "MAGI"

#[unsafe(no_mangle)]
pub(crate) static mut ISOLATED_PENDING: Option<PreparedTransition> = None;
pub(crate) static ISOLATED_CTL_PA: AtomicUsize = AtomicUsize::new(0);
pub(crate) static ISOLATED_DATA_PA: AtomicUsize = AtomicUsize::new(0);
pub(crate) static ISOLATED_HANDLE_ID: AtomicUsize = AtomicUsize::new(0);
pub(crate) static ISOLATED_HANDLE_GENERATION: AtomicUsize = AtomicUsize::new(0);
pub(crate) static ISOLATED_INSTANCE_SATP: AtomicUsize = AtomicUsize::new(0);
pub(crate) static ISOLATED_CORE_SATP_BEFORE: AtomicUsize = AtomicUsize::new(0);
pub(crate) static ISOLATED_CORE_SATP_AFTER: AtomicUsize = AtomicUsize::new(0);
pub(crate) static ISOLATED_OUTCOME: AtomicUsize = AtomicUsize::new(0);
pub(crate) static ISOLATED_FAULTED: AtomicUsize = AtomicUsize::new(0);

#[unsafe(no_mangle)]
pub(crate) static mut ISOLATED_S_REGS: [usize; 12] = [0; 12];
#[unsafe(no_mangle)]
pub(crate) static mut ISOLATED_TP_AFTER: usize = 0;
#[unsafe(no_mangle)]
pub(crate) static mut ISOLATED_GP_BEFORE: usize = 0;
#[unsafe(no_mangle)]
pub(crate) static mut ISOLATED_GP_MATCH: usize = 0;

pub(crate) static TIMER_COUNT: AtomicUsize = AtomicUsize::new(0);
pub(crate) static TIMER_HANDLER_SATP: AtomicUsize = AtomicUsize::new(0);
pub(crate) static TIMER_HANDLER_SP: AtomicUsize = AtomicUsize::new(0);

pub(crate) static FAULT_COUNT: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FAULT_TARGET_VA: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FAULT_TARGET_PA: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FAULT_HANDLER_SATP: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FAULT_HANDLER_SP: AtomicUsize = AtomicUsize::new(0);
pub(crate) static FAULT_SEPC: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn read_satp() -> usize {
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

pub(crate) fn read_sp() -> usize {
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

pub(crate) fn ctl_pa() -> *mut usize {
    ISOLATED_CTL_PA.load(Ordering::Acquire) as *mut usize
}

pub(crate) unsafe fn ctl_word(index: usize) -> usize {
    // SAFETY: caller guarantees the fixture was set up; identity PA view.
    unsafe { ctl_pa().add(index).read() }
}

pub(crate) unsafe fn ctl_set(index: usize, value: usize) {
    // SAFETY: 同上。
    unsafe { ctl_pa().add(index).write(value) };
}

/// 把一个刚分配的 heap 页登记为组件私有（摘掉所有活着 root 的 identity 别名 +
/// 排除出后续共享计划）。
pub(crate) fn claim_private_page(pa: usize) -> Result<(), &'static str> {
    kernel::memory::kernel_mappings::publish_private_backing(PhysicalRange {
        base: pa,
        size: 4096,
    })
    .map_err(|_| "isolated: publish private backing failed")
}

/// 夹具入口符号地址 → 实例 AS 里的私有 VA（夹具页映射在 `ISOLATED_FIXTURE_VA`）。
pub(crate) fn fixture_entry(symbol: usize) -> usize {
    let start = core::ptr::addr_of!(isolated_fixture_start) as usize;
    ISOLATED_FIXTURE_VA + (symbol - start)
}

pub(crate) fn take_pending() -> PreparedTransition {
    // SAFETY: single-threaded selftest; the probe/driver runs exactly once.
    let pending = unsafe { (*core::ptr::addr_of_mut!(ISOLATED_PENDING)).take() };
    match pending {
        Some(transition) => transition,
        None => fail("isolated: pending transition missing"),
    }
}

pub(crate) fn prepare_or_fail(
    handle: AddressSpaceHandle,
    entry: usize,
    interrupts_enabled: bool,
) -> PreparedTransition {
    let stack = VirtualRange {
        base: ISOLATED_STACK_BASE,
        size: ISOLATED_STACK_SIZE,
    };
    match isolated::prepare(
        handle,
        entry,
        stack,
        0,
        interrupts_enabled,
        isolated::EntryArgs::pair(0, 0),
    ) {
        Ok(transition) => transition,
        Err(IsolatedPrepareError::NoSuchSpace) => fail("isolated: prepare: no such space"),
        Err(IsolatedPrepareError::Retired) => fail("isolated: prepare: retired space"),
        Err(IsolatedPrepareError::Unsupported) => fail("isolated: prepare: unsupported"),
        Err(IsolatedPrepareError::EntryNotExecutable) => {
            fail("isolated: prepare: entry not executable")
        }
        Err(IsolatedPrepareError::StackNotWritable) => {
            fail("isolated: prepare: stack not writable")
        }
        Err(IsolatedPrepareError::InvalidStack) => fail("isolated: prepare: invalid stack"),
    }
}

pub(crate) struct IsolatedFixture {
    pub(crate) handle: AddressSpaceHandle,
}

/// 建立测试实例：**共享 Core 映射**的私有 AS + 私有夹具代码页 + 控制页 +
/// 组件栈（DATA 页故意留空）。夹具代码页从 Core 镜像页映射到实例私有 VA，
/// 因此组件入口的 PC 属于"组件私有可执行范围"（故障归因）。
pub(crate) fn isolated_fixture() -> Result<IsolatedFixture, &'static str> {
    if !address_space::isolation_capable() {
        return Err("isolated: this profile has no private address space backend");
    }
    let handle = address_space::create_isolated_address_space_for(ComponentId::from_raw(0x150))
        .map_err(|_| "isolated: create_isolated_address_space_for failed")?;

    let ctl_pa = kernel::memory::vm_page_alloc().map_err(|_| "isolated: ctl page alloc")?;
    let data_pa = kernel::memory::vm_page_alloc().map_err(|_| "isolated: data page alloc")?;
    let stack_pa = kernel::memory::vm_page_alloc().map_err(|_| "isolated: stack page alloc")?;
    for page in [ctl_pa, data_pa, stack_pa] {
        claim_private_page(page)?;
    }

    // 夹具代码页：`.S` 用 balign 4096 保证整页独占；实例 AS 里映射到
    // ISOLATED_FIXTURE_VA（同 PA），入口 VA 由 `fixture_entry` 换算。
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
        ISOLATED_FIXTURE_VA,
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

use super::*;

// ---------------------------------------------------------------------------
// Core-direct 探针：在实例 AS 里被组件直接调用（Core 代码共享映射；
// 无 satp 切换、无 trap 往返）。记录 satp / sp 并触碰 Core 全局状态。
// ---------------------------------------------------------------------------

pub(crate) static CORE_DIRECT_SATP: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CORE_DIRECT_SP: AtomicUsize = AtomicUsize::new(0);
pub(crate) static CORE_DIRECT_MARKER: AtomicUsize = AtomicUsize::new(0);

/// Core 代码，被共享 Core 映射带进实例 AS：调用它不切 satp、不进 trap。
#[unsafe(no_mangle)]
pub(crate) extern "C" fn selftest_core_direct_probe() -> usize {
    CORE_DIRECT_SATP.store(read_satp(), Ordering::Release);
    CORE_DIRECT_SP.store(read_sp(), Ordering::Release);
    // Core .data/.bss 全局状态（共享映射）可写。
    CORE_DIRECT_MARKER.fetch_add(1, Ordering::AcqRel) + 1
}
