use crate::riscv64::sv39::{PageTable, Pte, PteFlags};

const SV39_MODE: usize = 8;
const GIGAPAGE_SIZE: usize = 1 << 30;

const IDENTITY_FLAGS: PteFlags = PteFlags::R
    .union(PteFlags::W)
    .union(PteFlags::X)
    .union(PteFlags::A)
    .union(PteFlags::D);
const MMIO_FLAGS: PteFlags = PteFlags::R
    .union(PteFlags::W)
    .union(PteFlags::A)
    .union(PteFlags::D);

static mut ROOT_TABLE: PageTable = PageTable::empty();

pub fn get_root_table() -> &'static mut PageTable {
    unsafe { &mut *core::ptr::addr_of_mut!(ROOT_TABLE) }
}

pub unsafe fn init_identity() {
    let root = get_root_table();

    // Root-level leaf PTEs are 1 GiB Sv39 gigapages. The low gigapage covers
    // the QEMU MMIO area; finer-grained holes and permissions wait for Phase C.
    root.entries[0] = Pte::new_leaf_pa(0, MMIO_FLAGS);
    for index in 0..4 {
        let pa = 0x8000_0000 + index * GIGAPAGE_SIZE;
        root.entries[index + 2] = Pte::new_leaf_pa(pa, IDENTITY_FLAGS);
    }
}

pub unsafe fn activate() {
    let root_pa = get_root_table() as *const PageTable as usize;
    let satp = (SV39_MODE << 60) | (root_pa >> 12);

    unsafe {
        core::arch::asm!("sfence.vma", options(nostack, preserves_flags));
        core::arch::asm!(
            "csrw satp, {satp}",
            satp = in(reg) satp,
            options(nostack, preserves_flags),
        );
        core::arch::asm!("sfence.vma", options(nostack, preserves_flags));
    }
}
