//! C ABI buffer access in the actual component's execution domain.
use super::{endpoint::ExecutionDomain, registry};
use crate::{errno::Errno, memory::address_space};

#[cfg(any(
    feature = "vm-nommu",
    all(
        feature = "vm-mmu",
        any(target_arch = "riscv32", target_arch = "riscv64")
    )
))]
use crate::memory::address_space::MappingPermission;

pub(crate) struct Pinned {
    #[cfg(any(
        feature = "vm-nommu",
        all(
            feature = "vm-mmu",
            any(target_arch = "riscv32", target_arch = "riscv64")
        )
    ))]
    space: Option<address_space::Access>,
    user: bool,
}
impl Pinned {
    pub fn new(
        space: Option<address_space::AddressSpaceHandle>,
        user: bool,
    ) -> Result<Self, Errno> {
        #[cfg(any(
            feature = "vm-nommu",
            all(
                feature = "vm-mmu",
                any(target_arch = "riscv32", target_arch = "riscv64")
            )
        ))]
        {
            Ok(Self {
                space: space
                    .map(address_space::access)
                    .transpose()
                    .map_err(|_| Errno::EFAULT)?,
                user,
            })
        }
        #[cfg(not(any(
            feature = "vm-nommu",
            all(
                feature = "vm-mmu",
                any(target_arch = "riscv32", target_arch = "riscv64")
            )
        )))]
        {
            if space.is_some() {
                Err(Errno::ENOTSUP)
            } else {
                Ok(Self { user })
            }
        }
    }
    pub fn validate(&self, address: usize, len: usize, write: bool) -> Result<(), Errno> {
        if len == 0 {
            return Ok(());
        }
        if address == 0 || address.checked_add(len).is_none() {
            return Err(Errno::EFAULT);
        }
        #[cfg(any(
            feature = "vm-nommu",
            all(
                feature = "vm-mmu",
                any(target_arch = "riscv32", target_arch = "riscv64")
            )
        ))]
        if let Some(space) = &self.space {
            let permission = MappingPermission::READ
                | if write {
                    MappingPermission::WRITE
                } else {
                    MappingPermission::empty()
                }
                | if self.user {
                    MappingPermission::USER
                } else {
                    MappingPermission::empty()
                };
            if !space.valid(address, len, permission) {
                return Err(Errno::EFAULT);
            }
        }
        let _ = (self.user, write);
        Ok(())
    }
    pub fn read(&mut self, address: usize, bytes: &mut [u8]) -> Result<(), Errno> {
        self.copy(address, bytes, false)
    }
    fn copy(&mut self, address: usize, bytes: &mut [u8], write: bool) -> Result<(), Errno> {
        self.validate(address, bytes.len(), write)?;
        if bytes.is_empty() {
            return Ok(());
        }
        #[cfg(any(
            feature = "vm-nommu",
            all(
                feature = "vm-mmu",
                any(target_arch = "riscv32", target_arch = "riscv64")
            )
        ))]
        if let Some(space) = &mut self.space {
            return space
                .copy(address, bytes, write, self.user)
                .map_err(|_| Errno::EFAULT);
        }
        unsafe {
            if write {
                core::ptr::copy_nonoverlapping(bytes.as_ptr(), address as *mut u8, bytes.len());
            } else {
                core::ptr::copy_nonoverlapping(
                    address as *const u8,
                    bytes.as_mut_ptr(),
                    bytes.len(),
                );
            }
        }
        Ok(())
    }
    pub fn write(&mut self, address: usize, bytes: &[u8]) -> Result<(), Errno> {
        self.validate(address, bytes.len(), true)?;
        for (index, chunk) in bytes.chunks(1024).enumerate() {
            let mut local = [0; 1024];
            local[..chunk.len()].copy_from_slice(chunk);
            self.copy(address + index * 1024, &mut local[..chunk.len()], true)?;
        }
        Ok(())
    }
    pub fn put<T: Copy>(&mut self, address: *mut T, value: T) -> Result<(), Errno> {
        self.write(address as usize, unsafe {
            core::slice::from_raw_parts(
                core::ptr::addr_of!(value).cast::<u8>(),
                core::mem::size_of::<T>(),
            )
        })
    }
}

fn private() -> Result<Option<(address_space::AddressSpaceHandle, bool)>, Errno> {
    let Some(ctx) = crate::resource::RequestContext::ambient() else {
        return Ok(None);
    };
    let registry = registry::get_registry().lock();
    let record = registry.get(ctx.component).ok_or(Errno::EPERM)?;
    if record.execution_domain == ExecutionDomain::KernelNative {
        return Ok(None);
    }
    Ok(Some((
        record.address_space.ok_or(Errno::EPERM)?,
        record.execution_domain == ExecutionDomain::SandboxedNative,
    )))
}
fn current() -> Result<Pinned, Errno> {
    let (space, user) = private()?.map_or((None, false), |(space, user)| (Some(space), user));
    Pinned::new(space, user)
}
pub(crate) fn validate(address: usize, len: usize, write: bool) -> Result<(), Errno> {
    current()?.validate(address, len, write)
}
#[cfg(all(target_arch = "riscv64", feature = "supervisor", feature = "vm-mmu"))]
pub(crate) fn read(address: usize, bytes: &mut [u8]) -> Result<(), Errno> {
    current()?.read(address, bytes)
}
#[cfg(all(target_arch = "riscv64", feature = "supervisor", feature = "vm-mmu"))]
pub(crate) fn put<T: Copy>(address: *mut T, value: T) -> Result<(), Errno> {
    current()?.put(address, value)
}
