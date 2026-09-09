use arch::{CpuArch, ResetType, SystemReset};
use core::{arch::global_asm, mem::MaybeUninit};

const STACK_BYTES: usize = 4096;
const UNMAPPED_ADDRESS: usize = 0x4000_0000;

#[cfg(target_arch = "riscv32")]
const PTE_READ_ONLY: u32 = 0xc3;
#[cfg(target_arch = "riscv32")]
const PTE_READ_WRITE: u32 = 0xc7;

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

#[cfg(target_arch = "riscv32")]
static mut SV32_TEST_TABLE: [u32; 1024] = [0; 1024];

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

pub fn run() -> ! {
    kernel::log!("selftest", "ready");
    let mut command = [0u8; 64];
    let length = kernel::print::read_line(&mut command);
    kernel::printk!("\n");
    match &command[..length] {
        b"mapping" => mapping(),
        b"context-switch" => context_switch(),
        b"illegal-instruction" => illegal_instruction(),
        b"load-fault" => load_fault(),
        b"store-readonly" => store_readonly_fault(),
        b"execute-nx" => execute_nx_fault(),
        _ => fail("unknown command"),
    }
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

fn pass(name: &str) -> ! {
    kernel::log!("selftest", "{}: PASS", name);
    arch::ResetImpl::system_reset(ResetType::Shutdown)
}

fn fail(reason: &str) -> ! {
    kernel::log!("selftest", "FAIL: {}", reason);
    panic!("selftest failed: {}", reason)
}
