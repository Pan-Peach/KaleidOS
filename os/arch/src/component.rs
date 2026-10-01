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
    /// Size in bytes of the target object section; relocation writes must
    /// stay inside `[section_offset, section_offset + section_size)`.
    pub section_size: usize,
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

    /// Bind a linked kernel symbol into the address a component can call.
    ///
    /// This is a **component-import / callable binding**, not a VA→PA query:
    /// it normalizes the linker's kernel view (e.g. the RISC-V high-half VMA)
    /// into the address the component's execution view can branch to.  Runtime
    /// virtual→physical translation belongs to the mapping owner
    /// (`AddressSpaceBackend::translate`), never to this hook.
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
