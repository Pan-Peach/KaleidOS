//! `kcore_*` 导出 ABI（EXPORT_SYMBOL 教学版）。
//!
//! 声明即契约：名字必须与 Core `component/export.rs` 的白名单逐字节一致，签名
//! 错误 = UB（loader 只按名字精确解析，不校验签名）。这里保持**全量**声明，
//! 让各组件只共用这一份；新增 Core 导出时同步加在这里。

/// IRQ 投递回调：`ctx` 原样回传，Core 不解引用。
pub type IrqHandler = extern "C" fn(ctx: *mut ());

// 安全说明：以下符号由 Core 保证实现；调用方必须满足各自契约（指针有效性、
// out 参数可写、handle 归宿等），故调用点均为 `unsafe`。
unsafe extern "C" {
    // -- Runtime / shared heap（Core 共享堆，非 per-component 堆）--
    #[link_name = "kcore_heap_alloc"]
    pub fn kcore_heap_alloc(size: usize, align: usize) -> *mut u8;
    #[link_name = "kcore_heap_dealloc"]
    pub fn kcore_heap_dealloc(ptr: *mut u8, size: usize, align: usize) -> i32;

    // -- Logging / diagnostics --
    #[link_name = "kcore_console_write_byte"]
    pub fn kcore_console_write_byte(byte: u8);
    #[link_name = "kcore_log_line"]
    pub fn kcore_log_line(ptr: *const u8, len: usize) -> i32;

    // -- Machine query --
    #[link_name = "kcore_machine_boot_hart"]
    pub fn kcore_machine_boot_hart() -> u32;
    #[link_name = "kcore_machine_cpu_count"]
    pub fn kcore_machine_cpu_count() -> u32;
    #[link_name = "kcore_machine_has_hart"]
    pub fn kcore_machine_has_hart(hart_id: u32) -> i32;

    // -- System query --
    #[link_name = "kcore_free_page_count"]
    pub fn kcore_free_page_count() -> u32;
    #[link_name = "kcore_task_count"]
    pub fn kcore_task_count() -> u32;
    #[link_name = "kcore_component_count"]
    pub fn kcore_component_count() -> u32;

    // -- Component lifecycle --
    #[link_name = "kcore_component_load"]
    pub fn kcore_component_load(name: *const u8, len: usize) -> i32;
    #[link_name = "kcore_interface_publish"]
    pub fn kcore_interface_publish(
        name: *const u8,
        len: usize,
        kind: u32,
        abi: u64,
        api: *const (),
        ctx: *mut (),
    ) -> i32;
    #[link_name = "kcore_interface_available"]
    pub fn kcore_interface_available(name: *const u8, len: usize, kind: u32, abi: u64) -> i32;
    #[link_name = "kcore_interface_bind"]
    pub fn kcore_interface_bind(
        name: *const u8,
        len: usize,
        kind: u32,
        abi: u64,
        out_binding: *mut u64,
        out_api: *mut usize,
        out_ctx: *mut usize,
        out_generation: *mut u64,
    ) -> i32;
    #[link_name = "kcore_interface_refresh"]
    pub fn kcore_interface_refresh(
        binding: u64,
        abi: u64,
        out_api: *mut usize,
        out_ctx: *mut usize,
        out_generation: *mut u64,
    ) -> i32;

    // -- Task control --
    #[link_name = "kcore_task_create"]
    pub fn kcore_task_create(entry: usize) -> i32;
    #[link_name = "kcore_task_start"]
    pub fn kcore_task_start(id: u32) -> i32;
    #[link_name = "kcore_task_yield"]
    pub fn kcore_task_yield() -> i32;
    #[link_name = "kcore_task_exit"]
    pub fn kcore_task_exit() -> i32;
    #[link_name = "kcore_task_state"]
    pub fn kcore_task_state(id: u32) -> i32;

    // -- Panic containment --
    #[link_name = "kcore_panic_escape"]
    pub fn kcore_panic_escape() -> i32;

    // -- Scheduler --
    #[link_name = "kcore_sched_run"]
    pub fn kcore_sched_run() -> i32;

    // -- Resource authority: device discovery / MMIO --
    #[link_name = "kcore_device_nth"]
    pub fn kcore_device_nth(
        compatible: *const u8,
        len: usize,
        ordinal: u32,
        out_device_id: *mut u32,
    ) -> i32;
    #[link_name = "kcore_mmio_claim"]
    pub fn kcore_mmio_claim(device_id: u32, out_handle: *mut u64) -> i32;
    #[link_name = "kcore_mmio_read_u32"]
    pub fn kcore_mmio_read_u32(handle: u64, offset: u32, out_value: *mut u32) -> i32;
    #[link_name = "kcore_mmio_write_u32"]
    pub fn kcore_mmio_write_u32(handle: u64, offset: u32, value: u32) -> i32;
    #[link_name = "kcore_mmio_release"]
    pub fn kcore_mmio_release(handle: u64) -> i32;
    #[link_name = "kcore_mmio_lease"]
    pub fn kcore_mmio_lease(handle: u64, out_ptr: *mut usize, out_len: *mut usize) -> i32;

    // -- Resource authority: DMA --
    #[link_name = "kcore_dma_alloc"]
    pub fn kcore_dma_alloc(
        mmio_handle: u64,
        size: usize,
        direction: i32,
        out_handle: *mut u64,
    ) -> i32;
    #[link_name = "kcore_dma_lease"]
    pub fn kcore_dma_lease(
        handle: u64,
        out_ptr: *mut usize,
        out_len: *mut usize,
        out_device_addr: *mut u64,
    ) -> i32;
    #[link_name = "kcore_dma_release"]
    pub fn kcore_dma_release(handle: u64) -> i32;

    // -- Resource authority: IRQ --
    #[link_name = "kcore_irq_claim"]
    pub fn kcore_irq_claim(mmio_handle: u64, out_handle: *mut u64) -> i32;
    #[link_name = "kcore_irq_register"]
    pub fn kcore_irq_register(handle: u64, handler: IrqHandler, ctx: *mut ()) -> i32;
    #[link_name = "kcore_irq_enable"]
    pub fn kcore_irq_enable(handle: u64) -> i32;
    #[link_name = "kcore_irq_register_polled"]
    pub fn kcore_irq_register_polled(handle: u64) -> i32;
    #[link_name = "kcore_irq_poll"]
    pub fn kcore_irq_poll(handle: u64, out_count: *mut u64) -> i32;
    #[link_name = "kcore_irq_ack"]
    pub fn kcore_irq_ack(handle: u64) -> i32;
    #[link_name = "kcore_irq_release"]
    pub fn kcore_irq_release(handle: u64) -> i32;
}
