//! Mapping-level assertions shared by the Isolated ArchTest groups: install one
//! instance page, prove a VA is unmapped in an instance root.

pub(crate) fn map_instance_page(
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

pub(crate) fn assert_unmapped(handle: AddressSpaceHandle, va: usize, reason: &str) {
    if !matches!(address_space::translate(handle, va), Ok(None)) {
        fail(reason);
    }
}

use super::*;

/// **共享 Core 映射**：每个 Isolated AS 里 same VA → same PA；私有 VA 不共享。
pub(crate) fn isolated_shared_mappings() -> ! {
    if !address_space::isolation_capable() {
        fail("isolated-shared-mappings: no private address space backend");
    }
    let shared = kernel::memory::kernel_mappings::shared_mappings();
    if shared.is_empty() {
        fail("isolated-shared-mappings: boot installed no shared Core mappings");
    }
    let a = match address_space::create_isolated_address_space_for(ComponentId::from_raw(0x171)) {
        Ok(handle) => handle,
        Err(_) => fail("isolated-shared-mappings: create A failed"),
    };
    let b = match address_space::create_isolated_address_space_for(ComponentId::from_raw(0x172)) {
        Ok(handle) => handle,
        Err(_) => fail("isolated-shared-mappings: create B failed"),
    };

    // 抽查共享映射：A / B 都必须翻译到同一 PA。
    let mut checked = 0;
    for mapping in shared.iter().take(8) {
        let va = mapping.virtual_range.base;
        let pa = mapping.physical_range.base;
        match address_space::translate(a, va) {
            Ok(Some(found)) if found == pa => {}
            _ => fail("isolated-shared-mappings: A lost a shared Core mapping"),
        }
        match address_space::translate(b, va) {
            Ok(Some(found)) if found == pa => {}
            _ => fail("isolated-shared-mappings: B lost a shared Core mapping"),
        }
        checked += 1;
    }
    if checked == 0 {
        fail("isolated-shared-mappings: no shared mapping sampled");
    }

    // 私有布局 VA（夹具控制页 / 实例窗口）不在共享计划里。
    assert_unmapped(
        a,
        ISOLATED_CTL_VA,
        "isolated-shared-mappings: private VA is shared in A",
    );
    assert_unmapped(
        b,
        ISOLATED_CTL_VA,
        "isolated-shared-mappings: private VA is shared in B",
    );

    kernel::log!(
        "selftest",
        "isolated-shared-mappings: same VA->PA in kernel/AS_A/AS_B ({} shared ranges)",
        shared.len()
    );
    pass("isolated-shared-mappings")
}

/// **私有 backing 的别名排除**：A 先创建（已装 identity 共享映射），随后分配并
/// 发布 B 的私有 backing——A 与 B 都不得再经 identity 看见它；同一 backing 只能
/// 经 A 的私有映射到达。
pub(crate) fn isolated_private_unreachable() -> ! {
    if !address_space::isolation_capable() {
        fail("isolated-private-unreachable: no private address space backend");
    }
    // A 先于 backing 存在：它的共享计划里本来包含该 RAM 的 identity 别名。
    let a = match address_space::create_isolated_address_space_for(ComponentId::from_raw(0x173)) {
        Ok(handle) => handle,
        Err(_) => fail("isolated-private-unreachable: create A failed"),
    };
    let pa = match kernel::memory::vm_page_alloc() {
        Ok(pa) => pa,
        Err(_) => fail("isolated-private-unreachable: backing alloc failed"),
    };
    // 发布私有：摘掉所有活着 root 里的 identity 别名 + 排除出后续计划。
    if kernel::memory::kernel_mappings::publish_private_backing(PhysicalRange {
        base: pa,
        size: 4096,
    })
    .is_err()
    {
        fail("isolated-private-unreachable: private backing publish failed");
    }
    // B 在发布之后创建：共享计划里不应再有该 extent。
    let b = match address_space::create_isolated_address_space_for(ComponentId::from_raw(0x174)) {
        Ok(handle) => handle,
        Err(_) => fail("isolated-private-unreachable: create B failed"),
    };

    // identity VA == PA：两个 root 都不可见（A 是"已装别名"的动态排除证明）。
    assert_unmapped(
        a,
        pa,
        "isolated-private-unreachable: A still sees B's backing via identity",
    );
    assert_unmapped(
        b,
        pa,
        "isolated-private-unreachable: B sees its own backing via identity",
    );

    // 私有映射只进 A：A 可经私有 VA 到达，B 不可。
    if let Err(_) = map_instance_page(
        a,
        ISOLATED_CTL_VA,
        pa,
        MappingPermission::READ | MappingPermission::WRITE,
    ) {
        fail("isolated-private-unreachable: private mapping failed");
    }
    match address_space::translate(a, ISOLATED_CTL_VA) {
        Ok(Some(found)) if found == pa => {}
        _ => fail("isolated-private-unreachable: A lost its private mapping"),
    }
    assert_unmapped(
        b,
        ISOLATED_CTL_VA,
        "isolated-private-unreachable: B can reach A's private mapping",
    );

    kernel::log!(
        "selftest",
        "isolated-private-unreachable: privacy held (extent 0x{:x})",
        pa
    );
    pass("isolated-private-unreachable")
}

/// **Core 直接访问**：组件在实例 AS 里直接调用 Core 代码——satp 不切换、
/// 不进 trap、Core 全局状态与组件栈照常工作。
pub(crate) fn isolated_core_direct() -> ! {
    if !address_space::isolation_capable() {
        fail("isolated-core-direct: no private address space backend");
    }
    let handle =
        match address_space::create_isolated_address_space_for(ComponentId::from_raw(0x175)) {
            Ok(handle) => handle,
            Err(_) => fail("isolated-core-direct: create failed"),
        };
    let ctl_pa = match kernel::memory::vm_page_alloc() {
        Ok(pa) => pa,
        Err(_) => fail("isolated-core-direct: ctl page alloc"),
    };
    let stack_pa = match kernel::memory::vm_page_alloc() {
        Ok(pa) => pa,
        Err(_) => fail("isolated-core-direct: stack page alloc"),
    };
    for extent in [ctl_pa, stack_pa] {
        if kernel::memory::kernel_mappings::publish_private_backing(PhysicalRange {
            base: extent,
            size: 4096,
        })
        .is_err()
        {
            fail("isolated-core-direct: publish private page failed");
        }
        // SAFETY: 页来自 vm_page_alloc；identity 视图可写（Core root）。
        unsafe { core::ptr::write_bytes(extent as *mut u8, 0, 4096) };
    }
    if map_instance_page(
        handle,
        ISOLATED_CTL_VA,
        ctl_pa,
        MappingPermission::READ | MappingPermission::WRITE,
    )
    .is_err()
    {
        fail("isolated-core-direct: ctl map failed");
    }
    if map_instance_page(
        handle,
        ISOLATED_STACK_BASE,
        stack_pa,
        MappingPermission::READ | MappingPermission::WRITE,
    )
    .is_err()
    {
        fail("isolated-core-direct: stack map failed");
    }

    let stack = VirtualRange {
        base: ISOLATED_STACK_BASE,
        size: ISOLATED_STACK_SIZE,
    };
    let transition = match isolated::prepare(
        handle,
        isolated_core_direct_entry as *const () as usize,
        stack,
        0,
        false,
        isolated::EntryArgs::pair(0, 0),
    ) {
        Ok(transition) => transition,
        Err(_) => fail("isolated-core-direct: prepare failed"),
    };
    let instance_satp = transition.satp();
    CORE_DIRECT_MARKER.store(0, Ordering::Release);
    CORE_DIRECT_SATP.store(0, Ordering::Release);
    CORE_DIRECT_SP.store(0, Ordering::Release);

    let outcome = isolated::enter(transition);
    match outcome {
        Outcome::Returned(0x5d) => {}
        _ => fail("isolated-core-direct: component did not return"),
    }
    if CORE_DIRECT_SATP.load(Ordering::Acquire) != instance_satp {
        fail("isolated-core-direct: Core call switched satp");
    }
    let direct_sp = CORE_DIRECT_SP.load(Ordering::Acquire);
    if direct_sp < ISOLATED_STACK_BASE || direct_sp > ISOLATED_STACK_BASE + ISOLATED_STACK_SIZE {
        fail("isolated-core-direct: Core call did not run on the component stack");
    }
    if CORE_DIRECT_MARKER.load(Ordering::Acquire) != 1 {
        fail("isolated-core-direct: Core global state not usable");
    }
    // SAFETY: identity 视图读回控制页（Core 自己的视图）。
    let ctl = unsafe { core::slice::from_raw_parts(ctl_pa as *const usize, 8) };
    if ctl[0] != 0x5d5d {
        fail("isolated-core-direct: component did not run");
    }
    if ctl[1] != instance_satp {
        fail("isolated-core-direct: component did not observe the private root");
    }
    if ctl[4] != 1 {
        fail("isolated-core-direct: Core probe marker not returned");
    }

    kernel::log!(
        "selftest",
        "isolated-core-direct: direct OK (satp unchanged, Core state on component stack)"
    );
    pass("isolated-core-direct")
}
