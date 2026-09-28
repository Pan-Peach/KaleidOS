//! x86_64 component object ABI (skeleton; relocation not implemented).
//!
//! The ELF format is parsed by Core; this backend owns the x86_64 relocation
//! kinds (`R_X86_64_*`) and instruction patching.  Until implemented, `apply`
//! returns [`RelocationError::Unsupported`] instead of silently applying
//! RISC-V patching.

use crate::component::{Relocation, RelocationBackend, RelocationError, WordSize};

/// x86_64 relocation backend.
pub struct X86_64Relocator;

impl RelocationBackend for X86_64Relocator {
    /// `EM_X86_64`.
    const ELF_MACHINE: u16 = 62;

    fn new() -> Self {
        Self
    }

    fn normalize_symbol_address(address: usize) -> usize {
        // x86_64 kernel here has no high-half split; identity for now.
        // TODO(x86_64): mirror the real address scheme once the boot layout is defined.
        address
    }

    fn apply(
        &mut self,
        _width: WordSize,
        _image: &mut [u8],
        _image_base: usize,
        _relocation: Relocation,
        _symbol_address: usize,
    ) -> Result<(), RelocationError> {
        // TODO(x86_64): implement R_X86_64_* relocations.  Until then, refuse
        // rather than mis-apply another ISA's relocation semantics.
        Err(RelocationError::Unsupported)
    }
}
