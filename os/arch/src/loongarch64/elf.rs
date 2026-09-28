//! loongarch64 component object ABI (skeleton; relocation not implemented).
//!
//! Until implemented, `apply` returns [`RelocationError::Unsupported`] instead
//! of silently applying another ISA's patching.

use crate::component::{Relocation, RelocationBackend, RelocationError, WordSize};

/// loongarch64 relocation backend.
pub struct Loongarch64Relocator;

impl RelocationBackend for Loongarch64Relocator {
    /// `EM_LOONGARCH`.
    const ELF_MACHINE: u16 = 258;

    fn new() -> Self {
        Self
    }

    fn normalize_symbol_address(address: usize) -> usize {
        // TODO(loongarch64): mirror the real address scheme once the boot layout is defined.
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
        // TODO(loongarch64): implement R_LARCH_* relocations.
        Err(RelocationError::Unsupported)
    }
}
