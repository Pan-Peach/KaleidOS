//! aarch64 component object ABI (skeleton; relocation not implemented).
//!
//! Until implemented, `apply` returns [`RelocationError::Unsupported`] instead
//! of silently applying another ISA's patching.

use crate::component::{Relocation, RelocationBackend, RelocationError, WordSize};

/// aarch64 relocation backend.
pub struct Aarch64Relocator;

impl RelocationBackend for Aarch64Relocator {
    /// `EM_AARCH64`.
    const ELF_MACHINE: u16 = 183;

    fn new() -> Self {
        Self
    }

    fn normalize_symbol_address(address: usize) -> usize {
        // TODO(aarch64): mirror the real address scheme once the boot layout is defined.
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
        // TODO(aarch64): implement R_AARCH64_* relocations.
        Err(RelocationError::Unsupported)
    }
}
