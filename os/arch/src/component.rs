//! Architecture-facing contract for component object relocation.
//!
//! The ELF file format is parsed by Core.  Each architecture backend owns its
//! object ABI, relocation kinds, instruction patching, and linked-address
//! normalization.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WordSize {
    Bits32,
    Bits64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Relocation {
    pub target_section: usize,
    /// Offset within the target object section.
    pub section_offset: usize,
    /// Offset within the placed component image.
    pub image_offset: usize,
    pub kind: u32,
    pub addend: i64,
    pub symbol_section: usize,
    pub symbol_value: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelocationError {
    Unsupported,
    OutOfBounds,
    AddressOverflow,
}

/// Per-architecture component object ABI implementation.
pub trait RelocationBackend: Sized {
    const ELF_MACHINE: u16;

    fn new() -> Self;

    fn is_noop(kind: u32) -> bool {
        let _ = kind;
        false
    }

    /// Convert a linked kernel symbol into the address visible to a component.
    fn normalize_symbol_address(address: usize) -> usize;

    fn apply(
        &mut self,
        width: WordSize,
        image: &mut [u8],
        image_base: usize,
        relocation: Relocation,
        symbol_address: usize,
    ) -> Result<(), RelocationError>;
}
