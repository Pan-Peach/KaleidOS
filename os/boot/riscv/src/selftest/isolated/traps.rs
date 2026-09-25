//! Trap round trips inside a private AS: timer delivery, recoverable page fault,
//! and refusal to recover (abandon).  Fault dispatch must run on the Core root
//! and Core trap stack (shared by every Isolated root).

/// timer 用例：组件在私有 AS 里循环，时钟中断在组件上下文被接收，经
/// Core AS / Core trap 栈处理后再 `sret` 恢复组件。
pub(crate) fn isolated_timer() -> ! {
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

pub(crate) extern "C" fn isolated_timer_handler() {
    TIMER_COUNT.fetch_add(1, Ordering::AcqRel);
    TIMER_HANDLER_SATP.store(read_satp(), Ordering::Release);
    TIMER_HANDLER_SP.store(read_sp(), Ordering::Release);
    // SAFETY: identity PA view of the instance control page (single CPU).
    unsafe { ctl_set(CTL_FLAG, 1) };
    arch::TimerImpl::cancel_deadline();
}

/// fault 用例：组件在私有 AS 里访问未映射页 → gateway trap → Core 策略补映射
/// → 恢复后重试成功。
pub(crate) fn isolated_fault() -> ! {
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
pub(crate) fn isolated_fault_abandon() -> ! {
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

pub(crate) fn isolated_fault_policy(fault: &mut ComponentFault<'_>) -> FaultDecision {
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
pub(crate) fn assert_fault_ran_on_core_context(case: &str, core_satp: usize) {
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

use super::*;
