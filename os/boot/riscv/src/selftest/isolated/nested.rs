//! **嵌套 AS 切换**：Core/AS_A → AS_B → AS_A（CP6 的机制证明）。
//!
//! A 的入口在 AS_A 里**直接调用 Core helper**（共享 Core 映射）；helper 内再
//! `isolated::enter(B)`。trampoline 保存调用者（AS_A）现场，B 返回 / 被放弃后
//! 恢复 satp = AS_A 与调用者栈——Core 代码 / 栈 / 全局状态全程有效。
//!
//! - 健康路径：一次向外 + 一次返回，A 继续并正常返回 0x5e。
//! - 故障路径：B 跳进未映射地址 → 普通 trap 路径用 **B 的**跨 AS 现场归因 →
//!   Abandon → 只放弃 B，A 的现场原样恢复（A 仍返回 0x5e）。

use core::sync::atomic::{AtomicUsize, Ordering};

/// B 实例的控制页 / 栈 VA（A 用 `ISOLATED_CTL_VA` / `ISOLATED_STACK_BASE`）。
const NESTED_B_CTL_VA: usize = 0x3000_4000;
const NESTED_B_STACK_BASE: usize = 0x3000_5000;
const NESTED_STACK_SIZE: usize = 4096;

/// B 实例（`isolated::enter` 的嵌套输入；由 Core helper 取用）。
pub(crate) static mut ISOLATED_PENDING_B: Option<PreparedTransition> = None;

/// 共享 Core 边界栈（Core dispatch 在 AS_A 里进入 AS_B 时使用；`asm` 从
/// `NESTED_CORE_STACK_TOP` 读栈顶）。
#[repr(align(16))]
struct NestedCoreStack([u8; 16 * 1024]);
static mut NESTED_CORE_STACK: NestedCoreStack = NestedCoreStack([0; 16 * 1024]);
#[unsafe(no_mangle)]
pub(crate) static mut NESTED_CORE_STACK_TOP: usize = 0;

fn prepare_nested_core_stack() -> usize {
    // SAFETY: 只取静态数组地址（不读内容）。
    let base = unsafe { core::ptr::addr_of!(NESTED_CORE_STACK.0) } as usize;
    let top = (base + 16 * 1024) & !15;
    // SAFETY: 单线程 selftest；asm 通过该静态读栈顶。
    unsafe { core::ptr::addr_of_mut!(NESTED_CORE_STACK_TOP).write(top) };
    top
}

pub(crate) static NESTED_OUTER_SATP: AtomicUsize = AtomicUsize::new(0);
pub(crate) static NESTED_B_SATP: AtomicUsize = AtomicUsize::new(0);
pub(crate) static NESTED_AFTER_SATP: AtomicUsize = AtomicUsize::new(0);
pub(crate) static NESTED_B_OUTCOME: AtomicUsize = AtomicUsize::new(0);
pub(crate) static NESTED_OUTER_SP: AtomicUsize = AtomicUsize::new(0);

fn stack_range(base: usize) -> VirtualRange {
    VirtualRange {
        base,
        size: NESTED_STACK_SIZE,
    }
}

struct NestedFixture {
    handle: AddressSpaceHandle,
    ctl_pa: usize,
}

/// 建立嵌套用例的实例 AS：共享 Core 映射 + 私有控制页 / 组件栈。
fn nested_fixture(
    owner: u32,
    ctl_va: usize,
    stack_base: usize,
) -> Result<NestedFixture, &'static str> {
    let handle = address_space::create_isolated_address_space_for(ComponentId::from_raw(owner))
        .map_err(|_| "isolated-nested: create address space failed")?;
    let ctl_pa = kernel::memory::vm_page_alloc().map_err(|_| "isolated-nested: ctl alloc")?;
    let stack_pa = kernel::memory::vm_page_alloc().map_err(|_| "isolated-nested: stack alloc")?;
    for page in [ctl_pa, stack_pa] {
        claim_private_page(page)?;
    }
    map_instance_page(
        handle,
        ctl_va,
        ctl_pa,
        MappingPermission::READ | MappingPermission::WRITE,
    )?;
    map_instance_page(
        handle,
        stack_base,
        stack_pa,
        MappingPermission::READ | MappingPermission::WRITE,
    )?;
    // SAFETY: 两个页来自 vm_page_alloc；identity 视图可读写。
    unsafe {
        core::ptr::write_bytes(ctl_pa as *mut u8, 0, 4096);
        core::ptr::write_bytes(stack_pa as *mut u8, 0, 4096);
    }
    Ok(NestedFixture { handle, ctl_pa })
}

/// Core helper：在 AS_A 里进入 B（由 A 的入口直接调用）。
///
/// 记录进入前 / 嵌套返回后的 satp，证明确实是"一次向外 + 一次返回"且回到
/// **调用者的 AS**（A），而不是 Core root。
#[unsafe(no_mangle)]
pub(crate) extern "C" fn selftest_nested_enter_b() -> usize {
    // SAFETY: 单线程 selftest；A 的入口只调用一次。
    let pending = unsafe { (*core::ptr::addr_of_mut!(ISOLATED_PENDING_B)).take() };
    let Some(transition) = pending else {
        return 2;
    };
    NESTED_B_SATP.store(transition.satp(), Ordering::Release);
    NESTED_OUTER_SATP.store(read_satp(), Ordering::Release);
    NESTED_OUTER_SP.store(read_sp(), Ordering::Release);
    let outcome = isolated::enter(transition);
    NESTED_AFTER_SATP.store(read_satp(), Ordering::Release);
    match outcome {
        Outcome::Returned(value) => {
            NESTED_B_OUTCOME.store(value, Ordering::Release);
            0
        }
        Outcome::Faulted => {
            NESTED_B_OUTCOME.store(usize::MAX, Ordering::Release);
            1
        }
    }
}

fn nested_case(case: &str, b_entry: usize, expect_fault: bool) -> ! {
    let a = match nested_fixture(0x180, ISOLATED_CTL_VA, ISOLATED_STACK_BASE) {
        Ok(fixture) => fixture,
        Err(reason) => fail(reason),
    };
    let b = match nested_fixture(0x181, NESTED_B_CTL_VA, NESTED_B_STACK_BASE) {
        Ok(fixture) => fixture,
        Err(reason) => fail(reason),
    };

    let core_stack_top = prepare_nested_core_stack();
    // B 先准备，放进 pending（A 的入口里由 Core helper 取用）。
    let b_transition = match isolated::prepare(
        b.handle,
        b_entry,
        stack_range(NESTED_B_STACK_BASE),
        false,
        isolated::EntryArgs::pair(0, 0),
    ) {
        Ok(transition) => transition,
        Err(_) => fail("isolated-nested: B prepare failed"),
    };
    let b_satp = b_transition.satp();
    // SAFETY: 单线程；A 的入口只取一次。
    unsafe { (*core::ptr::addr_of_mut!(ISOLATED_PENDING_B)).replace(b_transition) };

    let a_transition = match isolated::prepare(
        a.handle,
        isolated_nested_a_entry as *const () as usize,
        stack_range(ISOLATED_STACK_BASE),
        false,
        isolated::EntryArgs::pair(0, 0),
    ) {
        Ok(transition) => transition,
        Err(_) => fail("isolated-nested: A prepare failed"),
    };
    let a_satp = a_transition.satp();
    if a_satp == b_satp {
        fail("isolated-nested: A and B roots are identical");
    }
    let core_satp = read_satp();
    // 嵌套故障归因需要普通 trap 路径的异常钩子（无策略 = Abandon）。
    isolated::install();
    NESTED_B_OUTCOME.store(usize::MAX, Ordering::Release);

    let outcome = isolated::enter(a_transition);
    if read_satp() != core_satp {
        fail("isolated-nested: Core satp not restored after A");
    }
    if outcome != Outcome::Returned(0x5e) {
        fail("isolated-nested: A did not return normally");
    }
    // SAFETY: 夹具控制页 backing 仍驻留；Core root 的 identity 视图。
    let a_slots = a.ctl_pa as *const usize;
    if unsafe { a_slots.add(CTL_SATP).read_volatile() } != a_satp {
        fail("isolated-nested: A did not observe its own root");
    }
    if unsafe { a_slots.add(CTL_FLAG).read_volatile() } != a_satp {
        fail("isolated-nested: nested return did not restore A's root");
    }
    if NESTED_OUTER_SATP.load(Ordering::Acquire) != a_satp {
        fail("isolated-nested: Core helper ran outside A's root");
    }
    if NESTED_OUTER_SP.load(Ordering::Acquire) < core_stack_top - 16 * 1024
        || NESTED_OUTER_SP.load(Ordering::Acquire) > core_stack_top
    {
        fail("isolated-nested: Core dispatch did not run on the Core boundary stack");
    }
    if NESTED_AFTER_SATP.load(Ordering::Acquire) != a_satp {
        fail("isolated-nested: B return did not restore A's root");
    }
    if NESTED_B_SATP.load(Ordering::Acquire) != b_satp {
        fail("isolated-nested: B did not observe its own root");
    }
    // A 的入口把 B 的结果写进 CTL_DATA：0 = B 正常返回，1 = B 被放弃。
    let b_status = unsafe { a_slots.add(CTL_DATA).read_volatile() };
    // SAFETY: B 的控制页 backing 仍驻留；Core root 的 identity 视图。
    let b_slots = b.ctl_pa as *const usize;
    if expect_fault {
        if b_status != 1 {
            fail("isolated-nested: B fault was not contained");
        }
        if NESTED_B_OUTCOME.load(Ordering::Acquire) != usize::MAX {
            fail("isolated-nested: B fault was not reported as Faulted");
        }
        if unsafe { b_slots.add(CTL_MAGIC).read_volatile() } != 0 {
            fail("isolated-nested: faulting B wrote its control page");
        }
    } else {
        if b_status != 0 {
            fail("isolated-nested: B did not return normally");
        }
        if NESTED_B_OUTCOME.load(Ordering::Acquire) != 0x5f {
            fail("isolated-nested: B reported an unexpected value");
        }
        // SAFETY: B 的控制页 backing 仍驻留。
        if unsafe { b_slots.add(CTL_SATP).read_volatile() } != b_satp
            || unsafe { b_slots.add(CTL_MAGIC).read_volatile() } != 0x5f5f
        {
            fail("isolated-nested: B did not run in its own root");
        }
    }
    kernel::log!(
        "selftest",
        "{}: A=0x{:x}, B=0x{:x}, b_status={}",
        case,
        a_satp,
        b_satp,
        b_status
    );
    pass(case)
}

/// 健康路径：A → B（正常返回）→ A。
pub(crate) fn isolated_nested_as() -> ! {
    nested_case(
        "isolated-nested-as",
        isolated_nested_b_entry as *const () as usize,
        false,
    )
}

/// 故障路径：A → B（未映射取指，被 Core 放弃）→ A 继续。
pub(crate) fn isolated_nested_fault() -> ! {
    nested_case(
        "isolated-nested-fault",
        isolated_nested_b_fault_entry as *const () as usize,
        true,
    )
}

use super::*;
