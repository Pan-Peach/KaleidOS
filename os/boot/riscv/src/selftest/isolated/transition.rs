//! Synchronous Core → private AS → Core round trip: register / runtime-slot
//! discipline and the component-visible private root.

/// 同步往返用例：寄存器纪律探针 → gateway → 组件 → Core 恢复。
pub(crate) fn isolated_transition() -> ! {
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
pub(crate) extern "C" fn isolated_roundtrip_driver() {
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
pub(crate) extern "C" fn selftest_isolated_roundtrip_resumed() -> ! {
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

use super::*;
