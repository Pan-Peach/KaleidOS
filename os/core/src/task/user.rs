//! Ordinary user programs owned by a personality task, not .kcomp instances.
//! All mappings/registers/task associations are Core truth. No PID/ELF/syscalls.

use crate::errno::Errno;
pub type Result<T> = core::result::Result<T, Errno>;
pub const USER_LIMIT: usize = 0x4000_0000;

pub fn valid_range(base: usize, len: usize) -> bool {
    base >= 4096 && base.checked_add(len).is_some_and(|end| end <= USER_LIMIT)
}

#[cfg(any(
    test,
    all(target_arch = "riscv64", feature = "supervisor", feature = "vm-mmu")
))]
fn valid_mapping(base: usize, len: usize, flags: u32) -> bool {
    len != 0
        && valid_range(base, len)
        && base.is_multiple_of(4096)
        && len.is_multiple_of(4096)
        && flags & !7 == 0
        && flags & 1 != 0
        && flags & 6 != 6
}

/// Validate a whole permission edit before changing any hardware mappings.
#[cfg(any(
    test,
    all(target_arch = "riscv64", feature = "supervisor", feature = "vm-mmu")
))]
fn protected_mappings(
    mappings: &[crate::memory::address_space::Mapping],
    base: usize,
    len: usize,
    flags: u32,
) -> Result<alloc::vec::Vec<crate::memory::address_space::Mapping>> {
    use crate::memory::address_space::{
        Mapping, MappingPermission as Permission, PhysicalRange, VirtualRange,
    };
    if !valid_mapping(base, len, flags) {
        return Err(Errno::EINVAL);
    }
    let end = base + len;
    let mut covered = 0;
    let mut result = alloc::vec::Vec::new();
    for mapping in mappings {
        let start = mapping.virtual_range.base;
        let finish = start + mapping.virtual_range.size;
        let cut_start = start.max(base);
        let cut_end = finish.min(end);
        // Reserve before pushing: user-controlled memory pressure is ENOMEM,
        // never a Core-critical allocator panic. An overlap adds at most 3 pieces.
        result
            .try_reserve(if cut_start < cut_end { 3 } else { 1 })
            .map_err(|_| Errno::ENOMEM)?;
        if cut_start >= cut_end {
            result.push(*mapping);
            continue;
        }
        covered += cut_end - cut_start;
        for (left, right, permission) in [
            (start, cut_start, mapping.permission),
            (
                cut_start,
                cut_end,
                Permission::from_bits_retain(flags as u8) | Permission::USER,
            ),
            (cut_end, finish, mapping.permission),
        ] {
            if left == right {
                continue;
            }
            result.push(Mapping {
                virtual_range: VirtualRange {
                    base: left,
                    size: right - left,
                },
                physical_range: PhysicalRange {
                    base: mapping.physical_range.base + left - start,
                    size: right - left,
                },
                permission,
            });
        }
    }
    if covered != len {
        return Err(Errno::EFAULT);
    }
    Ok(result)
}

#[cfg(all(target_arch = "riscv64", feature = "supervisor", feature = "vm-mmu"))]
mod implementation {
    use super::*;
    use crate::irq::IrqSaveGuard;
    use crate::memory::address_space::{
        self as spaces, AddressSpaceHandle, Mapping, MappingPermission as Permission,
        PhysicalRange, VirtualRange,
    };
    use crate::task::{TaskId, TaskState};
    use crate::{component::ComponentId, generated::abi::UserTrap, memory, sched, task};
    use alloc::{boxed::Box, vec::Vec};
    use arch::riscv::user::{UserContext, UserFrame};
    use core::sync::atomic::{AtomicUsize, Ordering};

    static ACTIVE: [AtomicUsize; crate::machine::MAX_CPUS] =
        [const { AtomicUsize::new(0) }; crate::machine::MAX_CPUS];

    #[derive(Debug, PartialEq)]
    pub struct UserDomain {
        space: AddressSpaceHandle,
        mappings: Vec<Mapping>,
        backing: Vec<memory::MemoryLease>,
        frame: UserFrame,
        prepared: bool,
        reply: bool,
        faulted: bool,
    }

    impl UserDomain {
        fn new(owner: ComponentId) -> Result<Self> {
            let space =
                spaces::create_isolated_address_space_for(owner).map_err(|_| Errno::ENOMEM)?;
            Ok(Self {
                space,
                mappings: Vec::new(),
                backing: Vec::new(),
                frame: UserFrame::default(),
                prepared: false,
                reply: false,
                faulted: false,
            })
        }

        fn map(&mut self, base: usize, len: usize, flags: u32) -> Result<()> {
            if !valid_mapping(base, len, flags) {
                return Err(Errno::EINVAL);
            }
            // Metadata must fit before either physical allocation or PTE commit.
            self.mappings.try_reserve(1).map_err(|_| Errno::ENOMEM)?;
            self.backing.try_reserve(1).map_err(|_| Errno::ENOMEM)?;
            let lease = memory::alloc_region(len).map_err(|_| Errno::ENOMEM)?;
            unsafe { core::ptr::write_bytes(lease.base() as *mut u8, 0, lease.size()) };
            let permission = Permission::from_bits_retain(flags as u8) | Permission::USER;
            let mapping = Mapping {
                virtual_range: VirtualRange { base, size: len },
                physical_range: PhysicalRange {
                    base: lease.base(),
                    size: len,
                },
                permission,
            };
            spaces::map(self.space, mapping).map_err(|error| match error {
                spaces::MapError::OutOfMemory | spaces::MapError::BackendFailed => Errno::ENOMEM,
                _ => Errno::EINVAL,
            })?;
            self.mappings.push(mapping);
            self.backing.push(lease);
            Ok(())
        }

        fn chunks(
            &self,
            base: usize,
            len: usize,
            permission: Permission,
        ) -> Result<Vec<(usize, usize)>> {
            if !valid_range(base, len) || self.faulted {
                return Err(Errno::EFAULT);
            }
            let mut chunks = Vec::new();
            let mut offset = 0;
            while offset < len {
                let address = base + offset;
                let mapping = self
                    .mappings
                    .iter()
                    .find(|m| {
                        m.permission.contains(permission | Permission::USER)
                            && address >= m.virtual_range.base
                            && address < m.virtual_range.base + m.virtual_range.size
                    })
                    .ok_or(Errno::EFAULT)?;
                let count = (len - offset)
                    .min(4096 - address % 4096)
                    .min(mapping.virtual_range.base + mapping.virtual_range.size - address);
                let physical = spaces::translate(self.space, address)
                    .map_err(|_| Errno::EFAULT)?
                    .ok_or(Errno::EFAULT)?;
                chunks.try_reserve(1).map_err(|_| Errno::ENOMEM)?;
                chunks.push((physical, count));
                offset += count;
            }
            Ok(chunks)
        }

        fn prepare(&mut self, pc: usize, sp: usize) -> Result<()> {
            if !sp.is_multiple_of(16) || sp < 16 {
                return Err(Errno::EINVAL);
            }
            self.chunks(pc, 2, Permission::READ | Permission::EXECUTE)?;
            self.chunks(sp - 16, 16, Permission::READ | Permission::WRITE)?;
            let stack = self
                .mappings
                .iter()
                .find(|m| {
                    m.virtual_range.base <= sp - 16
                        && sp <= m.virtual_range.base + m.virtual_range.size
                })
                .ok_or(Errno::EFAULT)?;
            if stack.permission.contains(Permission::EXECUTE) {
                return Err(Errno::EINVAL);
            }
            self.frame = UserFrame::default();
            self.frame.pc = pc;
            self.frame.x[2] = sp;
            self.prepared = true;
            self.reply = false;
            Ok(())
        }
    }

    impl Drop for UserDomain {
        fn drop(&mut self) {
            // No task can activate this root after removal. Never expose its
            // physical backing to a component. Published tasks stay resident.
            let _ = spaces::retire(self.space);
        }
    }

    fn accessible(
        owner: ComponentId,
        record: &task::TaskRecord,
        id: TaskId,
        current: Option<TaskId>,
    ) -> Result<()> {
        if record.owner() != owner {
            return Err(Errno::EPERM);
        }
        if record.state() != TaskState::Created && current != Some(id) {
            return Err(Errno::EBUSY);
        }
        Ok(())
    }

    pub fn create(owner: ComponentId, entry: usize, arg: *mut ()) -> Result<u32> {
        let domain = Box::new(UserDomain::new(owner)?);
        let id = task::create_task(owner, entry, arg).map_err(Errno::from)?;
        task::get_task_table().lock().get_mut(id).unwrap().user = Some(domain);
        Ok(id.raw())
    }

    pub fn map(owner: ComponentId, raw: u32, base: usize, len: usize, flags: u32) -> Result<()> {
        let id = TaskId::from_raw(raw);
        let current = sched::current_task();
        let mut table = task::get_task_table().lock();
        let record = table.get_mut(id).ok_or(Errno::ESRCH)?;
        accessible(owner, record, id, current)?;
        record
            .user
            .as_mut()
            .ok_or(Errno::EINVAL)?
            .map(base, len, flags)
    }

    /// Kernel buffers are borrowed by the trusted C caller. Whole user range
    /// is validated before copying; the task lock serializes all mapping edits.
    /// # Safety
    /// The kernel buffer must be valid for len bytes in the copy direction;
    /// it must not alias the user backing. Unsupported targets never access it.
    pub unsafe fn copy(
        owner: ComponentId,
        raw: u32,
        base: usize,
        buffer: *mut u8,
        len: usize,
        direction: u32,
    ) -> Result<()> {
        let id = TaskId::from_raw(raw);
        let current = sched::current_task();
        let mut table = task::get_task_table().lock();
        let record = table.get_mut(id).ok_or(Errno::ESRCH)?;
        accessible(owner, record, id, current)?;
        if direction == 2 && record.state() != TaskState::Created {
            return Err(Errno::EPERM);
        }
        let domain = record.user.as_ref().ok_or(Errno::EINVAL)?;
        let permission = if direction == 1 {
            Permission::WRITE
        } else {
            Permission::READ
        };
        let chunks = domain.chunks(base, len, permission)?;
        let mut offset = 0;
        for (physical, count) in chunks {
            if direction == 0 {
                unsafe {
                    core::ptr::copy_nonoverlapping(physical as *const u8, buffer.add(offset), count)
                };
            } else {
                unsafe {
                    core::ptr::copy_nonoverlapping(buffer.add(offset), physical as *mut u8, count)
                };
            }
            offset += count;
        }
        Ok(())
    }

    pub fn prepare(owner: ComponentId, raw: u32, pc: usize, sp: usize) -> Result<()> {
        let mut table = task::get_task_table().lock();
        let record = table.get_mut(TaskId::from_raw(raw)).ok_or(Errno::ESRCH)?;
        if record.owner() != owner || record.state() != TaskState::Created {
            return Err(Errno::EPERM);
        }
        record.user.as_mut().ok_or(Errno::EINVAL)?.prepare(pc, sp)
    }

    pub fn protect(
        owner: ComponentId,
        raw: u32,
        base: usize,
        len: usize,
        flags: u32,
    ) -> Result<()> {
        let id = TaskId::from_raw(raw);
        let current = sched::current_task();
        let mut table = task::get_task_table().lock();
        let record = table.get_mut(id).ok_or(Errno::ESRCH)?;
        accessible(owner, record, id, current)?;
        let domain = record.user.as_mut().ok_or(Errno::EINVAL)?;
        let mappings = protected_mappings(&domain.mappings, base, len, flags)?;
        let staged = spaces::create_isolated_address_space_for(owner).map_err(|_| Errno::ENOMEM)?;
        // Build a complete root first. A mapping failure leaves the old AS and
        // permissions unchanged; no partial unmap/re-map can escape to U-mode.
        for mapping in &mappings {
            if spaces::map(staged, *mapping).is_err() {
                let _ = spaces::retire(staged);
                return Err(Errno::ENOMEM);
            }
        }
        let old = core::mem::replace(&mut domain.space, staged);
        domain.mappings = mappings;
        let _ = spaces::retire(old);
        Ok(())
    }

    pub fn step(owner: ComponentId, result: i64, deadline: u64) -> Result<UserTrap> {
        if crate::component::containment::scheduling_forbidden() {
            return Err(Errno::EPERM);
        }
        let _irq = IrqSaveGuard::new();
        let id = sched::current_task().ok_or(Errno::EPERM)?;
        let cpu = crate::smp::current_cpu();
        let (domain, activation) = {
            let mut table = task::get_task_table().lock();
            let record = table.get_mut(id).ok_or(Errno::ESRCH)?;
            if record.owner() != owner || record.state() != TaskState::Running(cpu) {
                return Err(Errno::EPERM);
            }
            let domain = record.user.as_mut().ok_or(Errno::EINVAL)?;
            if !domain.prepared || domain.faulted {
                return Err(Errno::EINVAL);
            }
            if domain.reply {
                domain.frame.x[10] = result as usize;
            }
            domain.reply = false;
            let activation = spaces::prepare_activation(domain.space).map_err(|_| Errno::EFAULT)?;
            (&mut **domain as *mut UserDomain, activation.token())
        };
        if deadline <= <arch::TimerImpl as arch::Timer>::now() {
            return Err(Errno::EINVAL);
        }
        crate::component::isolated::install();
        let _deadline = crate::timer::execution_deadline(deadline).map_err(|_| Errno::ENOTSUP)?;
        let mut context =
            UserContext::new(activation.satp(), unsafe { &mut (*domain).frame }, id.raw());
        let slot = &ACTIVE[cpu.raw()];
        if slot.swap(&mut context as *mut _ as usize, Ordering::AcqRel) != 0 {
            panic!("nested user execution");
        }
        unsafe { arch::riscv::user::run(&mut context) };
        slot.store(0, Ordering::Release);
        let domain = unsafe { &mut *domain };
        let frame = &mut domain.frame;
        let event = UserTrap {
            task: id.raw(),
            reserved: 0,
            cause: context.cause as u64,
            pc: frame.pc as u64,
            address: context.address as u64,
            number: frame.x[17] as u64,
            arg0: frame.x[10] as u64,
            arg1: frame.x[11] as u64,
            arg2: frame.x[12] as u64,
            arg3: frame.x[13] as u64,
            arg4: frame.x[14] as u64,
            arg5: frame.x[15] as u64,
        };
        if context.cause == 8 {
            frame.pc = frame.pc.checked_add(4).ok_or(Errno::EFAULT)?;
            domain.reply = true;
        } else if context.cause >> 63 == 0 {
            domain.faulted = true;
        }
        Ok(event)
    }

    /// # Safety
    /// frame must be the live frame supplied by the Arch trap hook on this CPU.
    pub(crate) unsafe fn on_trap(
        frame: *mut arch::riscv::trap::TrapFrame,
        cause: usize,
        address: usize,
    ) -> bool {
        let trap = unsafe { &*frame };
        if trap.status & 0x100 != 0 {
            return false;
        }
        let pointer = ACTIVE[crate::smp::current_cpu().raw()].load(Ordering::Acquire);
        if pointer == 0 {
            return false;
        }
        let context = unsafe { &*(pointer as *const UserContext) };
        if sched::current_task().map(TaskId::raw) != Some(context.task)
            || arch::riscv::mmu::current_satp() != context.satp
        {
            return false;
        }
        unsafe { arch::riscv::user::stop(pointer as *mut UserContext, trap, cause, address) }
    }

    pub fn clone_current(owner: ComponentId, entry: usize, arg: *mut ()) -> Result<u32> {
        let source = sched::current_task().ok_or(Errno::EPERM)?;
        let mut domain = Box::new(UserDomain::new(owner)?);
        {
            let table = task::get_task_table().lock();
            let record = table.get(source).ok_or(Errno::ESRCH)?;
            if record.owner() != owner {
                return Err(Errno::EPERM);
            }
            let original = record.user.as_ref().ok_or(Errno::EINVAL)?;
            if !original.reply || original.faulted {
                return Err(Errno::EINVAL);
            }
            for mapping in &original.mappings {
                domain.map(
                    mapping.virtual_range.base,
                    mapping.virtual_range.size,
                    (mapping.permission.bits() & 7) as u32,
                )?;
                let new = domain.mappings.last().unwrap();
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        mapping.physical_range.base as *const u8,
                        new.physical_range.base as *mut u8,
                        mapping.virtual_range.size,
                    )
                };
            }
            domain.frame = original.frame.clone();
            domain.prepared = true;
            domain.reply = true;
        }
        let id = task::create_task(owner, entry, arg).map_err(Errno::from)?;
        task::get_task_table().lock().get_mut(id).unwrap().user = Some(domain);
        Ok(id.raw())
    }

    pub fn replace(owner: ComponentId, raw: u32) -> Result<()> {
        let current = sched::current_task().ok_or(Errno::EPERM)?;
        let staged = TaskId::from_raw(raw);
        if current == staged {
            return Err(Errno::EINVAL);
        }
        let mut table = task::get_task_table().lock();
        for id in [current, staged] {
            if table.get(id).is_none_or(|record| record.owner() != owner) {
                return Err(Errno::EPERM);
            }
        }
        let source = table.get(staged).unwrap();
        if source.state() != TaskState::Created
            || source.user.as_ref().is_none_or(|domain| !domain.prepared)
        {
            return Err(Errno::EINVAL);
        }
        let target = table.get(current).unwrap();
        if target
            .user
            .as_ref()
            .is_none_or(|domain| !domain.reply || domain.faulted)
        {
            return Err(Errno::EINVAL);
        }
        table
            .get_mut(current)
            .unwrap()
            .retired_user
            .try_reserve(1)
            .map_err(|_| Errno::ENOMEM)?;
        let mut source = table.remove(staged).map_err(Errno::from)?;
        let target = table.get_mut(current).unwrap();
        let old = target.user.replace(source.user.take().unwrap()).unwrap();
        let _ = spaces::retire(old.space);
        target.retired_user.push(*old); // Published backing remains resident.
        Ok(())
    }

    pub fn discard(owner: ComponentId, raw: u32) -> Result<()> {
        let id = TaskId::from_raw(raw);
        let mut table = task::get_task_table().lock();
        let record = table.get(id).ok_or(Errno::ESRCH)?;
        if record.owner() != owner || record.state() != TaskState::Created || record.user.is_none()
        {
            return Err(Errno::EPERM);
        }
        table.remove(id).map_err(Errno::from)?;
        Ok(())
    }
}

#[cfg(all(target_arch = "riscv64", feature = "supervisor", feature = "vm-mmu"))]
pub use implementation::*;

#[cfg(not(all(target_arch = "riscv64", feature = "supervisor", feature = "vm-mmu")))]
mod unsupported {
    use super::*;
    use crate::{component::ComponentId, generated::abi::UserTrap};
    pub fn create(_: ComponentId, _: usize, _: *mut ()) -> Result<u32> {
        Err(Errno::ENOTSUP)
    }
    pub fn map(_: ComponentId, _: u32, _: usize, _: usize, _: u32) -> Result<()> {
        Err(Errno::ENOTSUP)
    }
    /// # Safety
    /// The kernel buffer must be valid for len bytes in the copy direction;
    /// it must not alias the user backing. Unsupported targets never access it.
    pub unsafe fn copy(
        _: ComponentId,
        _: u32,
        _: usize,
        _: *mut u8,
        _: usize,
        _: u32,
    ) -> Result<()> {
        Err(Errno::ENOTSUP)
    }
    pub fn prepare(_: ComponentId, _: u32, _: usize, _: usize) -> Result<()> {
        Err(Errno::ENOTSUP)
    }
    pub fn protect(_: ComponentId, _: u32, _: usize, _: usize, _: u32) -> Result<()> {
        Err(Errno::ENOTSUP)
    }
    pub fn step(_: ComponentId, _: i64, _: u64) -> Result<UserTrap> {
        Err(Errno::ENOTSUP)
    }
    pub fn clone_current(_: ComponentId, _: usize, _: *mut ()) -> Result<u32> {
        Err(Errno::ENOTSUP)
    }
    pub fn replace(_: ComponentId, _: u32) -> Result<()> {
        Err(Errno::ENOTSUP)
    }
    pub fn discard(_: ComponentId, _: u32) -> Result<()> {
        Err(Errno::ENOTSUP)
    }
}
#[cfg(not(all(target_arch = "riscv64", feature = "supervisor", feature = "vm-mmu")))]
pub use unsupported::*;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn permission_edits_validate_the_whole_range_and_preserve_backing() {
        use crate::memory::address_space::{
            Mapping, MappingPermission as P, PhysicalRange, VirtualRange,
        };
        let source = [Mapping {
            virtual_range: VirtualRange {
                base: 0x1000,
                size: 0x3000,
            },
            physical_range: PhysicalRange {
                base: 0x8000_0000,
                size: 0x3000,
            },
            permission: P::READ | P::WRITE | P::USER,
        }];
        let result = protected_mappings(&source, 0x2000, 0x1000, 1).unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[1].physical_range.base, 0x8000_1000);
        assert_eq!(result[1].permission, P::READ | P::USER);
        assert_eq!(result[0].permission, source[0].permission);
        assert_eq!(result[2].permission, source[0].permission);
        assert_eq!(
            protected_mappings(&source, 0x1000, 0x4000, 1),
            Err(Errno::EFAULT)
        );
        for (base, len, flags) in [
            (0x1001, 0x1000, 1),
            (0x1000, 1, 1),
            (0x1000, 0x1000, 7),
            (0x1000, 0x1000, 2),
            (0x1000, 0x1000, 9),
            (usize::MAX - 4095, 4096, 1),
        ] {
            assert_eq!(
                protected_mappings(&source, base, len, flags),
                Err(Errno::EINVAL)
            );
        }
    }
    #[test]
    fn user_ranges_reject_null_kernel_addresses_and_wraparound() {
        assert!(valid_range(4096, 4096));
        assert!(valid_range(USER_LIMIT - 4096, 4096));
        assert!(!valid_range(0, 4096));
        assert!(!valid_range(USER_LIMIT - 1, 2));
        assert!(!valid_range(usize::MAX - 7, 16));
        assert!(!valid_range(0x8000_0000, 1));
    }
}
