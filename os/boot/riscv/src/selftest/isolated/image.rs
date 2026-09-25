//! Per-domain loading of a real `.kcomp` into a private AS: segment placement,
//! page-level permission enforcement, and Core-only mappings staying unreachable.

// -----------------------------------------------------------------------
// 真实 `.kcomp` 的按域装载 + 页级权限强制。
//
// 夹具 `kcomp_isolated` 零依赖 / 零 import；本模块把它的字节从内嵌 kpkg 读出，
// 走 `component::isolated_load` 放进一个只含「该镜像各段 + gateway 两页 +
// 实例栈 + 控制页」的私有 AS，再经 assembly gateway 进入。**这条用例直接
// 驱动机制**：不经任何组件创建路径。
// -----------------------------------------------------------------------

/// `kcomp_isolated` 的控制页协议槽号（与组件源码逐槽一致）。
pub(crate) const IMAGE_CTL_COMMAND: usize = 2;
pub(crate) const IMAGE_CTL_STATUS: usize = 3;
pub(crate) const IMAGE_CTL_TEXT_VA: usize = 4;
pub(crate) const IMAGE_CTL_DATA_VA: usize = 5;
pub(crate) const IMAGE_CTL_RODATA_VA: usize = 6;
pub(crate) const IMAGE_CTL_RODATA_VALUE: usize = 7;
pub(crate) const IMAGE_CTL_DATA_VALUE: usize = 8;
pub(crate) const IMAGE_CTL_BSS_VALUE: usize = 9;
pub(crate) const IMAGE_CTL_TARGET_VA: usize = 10;
/// 期望的实例 satp（Core 写；组件用它做环境门禁——见组件源码）。
pub(crate) const IMAGE_CTL_EXPECT_SATP: usize = 11;

pub(crate) const IMAGE_MAGIC: usize = 0x4953_4f4c; // "ISOL"
pub(crate) const IMAGE_RODATA_MAGIC: usize = 0x524f_4441; // "RODA"
/// 组件 `report()` 写进 data 段再读回的值（`DATA_CELL ^ 0x5555`）。
pub(crate) const IMAGE_DATA_STORED: usize = 0x4441_5441 ^ 0x5555;
pub(crate) const IMAGE_BSS_STORED: usize = 0x4242_5353;
pub(crate) const IMAGE_CANARY_MAGIC: usize = 0x4341_4e41; // "CANA"

pub(crate) const CMD_REPORT: usize = 0;
pub(crate) const CMD_STORE_TEXT: usize = 1;
pub(crate) const CMD_FETCH_DATA: usize = 2;
pub(crate) const CMD_LOAD_TARGET: usize = 3;

pub(crate) const R_X: MappingPermission = MappingPermission::READ.union(MappingPermission::EXECUTE);
pub(crate) const R_W: MappingPermission = MappingPermission::READ.union(MappingPermission::WRITE);

pub(crate) static IMAGE_FAULT_CAUSE: AtomicUsize = AtomicUsize::new(0);
pub(crate) static IMAGE_FAULT_STVAL: AtomicUsize = AtomicUsize::new(0);
pub(crate) static IMAGE_FAULT_PAGE_PA: AtomicUsize = AtomicUsize::new(0);

/// 按域装载夹具：真实 `.kcomp` 已落进实例 AS + 控制页 + 实例栈 + 一个
/// **Core 专属**金丝雀页（故意不映射进实例 AS）。
pub(crate) struct IsolatedImageFixture {
    handle: AddressSpaceHandle,
    image: PlacedImage,
    ctl_pa: usize,
    canary_pa: usize,
}
pub(crate) fn isolated_image_fixture() -> Result<IsolatedImageFixture, &'static str> {
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

    let ctl_pa = kernel::memory::vm_page_alloc().map_err(|_| "isolated-image: ctl page alloc")?;
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

pub(crate) fn image_segment_of(image: &PlacedImage, address: usize) -> Option<PlacedSegment> {
    image.segments().iter().copied().find(|segment| {
        address >= segment.virtual_range.base
            && address < segment.virtual_range.base + segment.virtual_range.size
    })
}

pub(crate) fn image_segment_permission(
    image: &PlacedImage,
    address: usize,
) -> Option<MappingPermission> {
    image_segment_of(image, address).map(|segment| segment.permission)
}

/// `va` 所在页必须翻译到该段 backing 的对应页（VA→PA 真相）。
pub(crate) fn image_page_is_backed(
    handle: AddressSpaceHandle,
    image: &PlacedImage,
    va: usize,
) -> bool {
    let page = va & !4095;
    let Some(segment) = image_segment_of(image, page) else {
        return false;
    };
    let expected =
        image.mapping(&segment).physical_range.base + (page - segment.virtual_range.base);
    matches!(address_space::translate(handle, page), Ok(Some(pa)) if pa == expected)
}
/// 主用例：真实 `.kcomp` 的代码在私有 AS 里经 gateway 跑完并返回；data /
/// rodata 在各自映射 VA 上可读；实例 AS 只含该镜像各段 + gateway 机制页 +
/// 实例栈 / 控制页，Core 专属映射不可达。
pub(crate) fn isolated_image() -> ! {
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
        core::ptr::addr_of!(super::super::MAPPING_VALUE) as usize,
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
    if unsafe { core::ptr::read_volatile(fixture.canary_pa as *const usize) } != IMAGE_CANARY_MAGIC
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
pub(crate) fn isolated_image_wrong_env() -> ! {
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
pub(crate) struct FaultObservation {
    fixture: IsolatedImageFixture,
    cause: usize,
    stval: usize,
    page_pa: usize,
}

pub(crate) fn enter_expecting_fault(
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
pub(crate) fn isolated_remember_fault_frame(fault: &mut ComponentFault<'_>) -> FaultDecision {
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
pub(crate) fn assert_permission_fault(
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
pub(crate) fn isolated_perm_text() -> ! {
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
pub(crate) fn isolated_perm_data() -> ! {
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
pub(crate) fn isolated_core_unreachable() -> ! {
    let observation = enter_expecting_fault("isolated-core-unreachable", CMD_LOAD_TARGET, |f| {
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

use super::*;
