//! Component loader: place a parsed ELF object and start its component entry.
//!
//! ELF structure lives in [`super::elf`].  Architecture-specific relocation
//! and linked-address handling live in the selected `arch` backend; this module owns the Core
//! policy around memory, exports, and component entry points.

use super::elf::{ElfClass, ElfError, ElfObject, Relocation as ElfRelocation, Section};
use crate::memory;
use alloc::vec::Vec;
use arch::ComponentRelocationImpl;
use arch::component::{Relocation, RelocationBackend, RelocationError, WordSize};

/// 加载失败原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoaderError {
    BadMagic,
    UnsupportedFormat,
    NotRelocatable,
    MachineMismatch,
    NoTextSection,
    NoEntrySymbol,
    UnsupportedRelocation,
    UnresolvedSymbol,
    OutOfMemory,
}

impl From<ElfError> for LoaderError {
    fn from(error: ElfError) -> Self {
        match error {
            ElfError::BadMagic => Self::BadMagic,
            ElfError::NotRelocatable => Self::NotRelocatable,
            ElfError::UnsupportedRelocation => Self::UnsupportedRelocation,
            ElfError::UnsupportedFormat => Self::UnsupportedFormat,
        }
    }
}

/// 加载完成的组件：代码已在内存中，入口已定位（未调用）。
#[derive(Debug, PartialEq, Eq)]
pub struct LoadedComponent {
    pub base: usize,
    /// 加载入口（`kcomp_init`，Linux `module_init` 类比）地址。
    pub entry: usize,
    /// 可选退出入口（`kcomp_exit`，Linux `module_exit` 类比）地址。
    ///
    /// 组件**可以**导出该 C ABI 符号（`extern "C" fn() -> i32`）；Core 解析它，
    /// 由停止路径（`component/exit.rs::stop_component`）在 Core-owned 隔离栈上
    /// 调用。`None` = 组件没有退出钩子（正常情况，停止时跳过）。
    pub exit: Option<usize>,
    pub text_size: usize,
    pub(crate) memory: Option<memory::MemoryLease>,
}

impl LoadedComponent {
    pub(crate) fn take_memory(&mut self) -> Option<memory::MemoryLease> {
        self.memory.take()
    }
}

/// 解析 ET_REL、放置 ALLOC 段、应用当前 ABI 重定位并定位入口。
///
/// 加载入口 `kcomp_init` 必须存在；退出入口 `kcomp_exit`（Linux `module_exit`
/// 类比）**可选**——存在则解析其地址存入 [`LoadedComponent::exit`]，不存在则为
/// `None`。Core 记录该地址，由停止路径在 Core-owned 隔离栈上调用。
pub fn load_component(blob: &[u8]) -> Result<LoadedComponent, LoaderError> {
    let object = ElfObject::parse(blob)?;
    if object.machine() != ComponentRelocationImpl::ELF_MACHINE {
        return Err(LoaderError::MachineMismatch);
    }

    let relocations = object.relocations()?;
    let (image_size, seg_place) = place_alloc_sections(object.sections())?;
    if seg_place.is_empty() {
        return Err(LoaderError::NoTextSection);
    }

    let symbol_table = object.symbol_table_index()?;
    let entry_offset = function_symbol_offset(&object, symbol_table, b"kcomp_init")?
        .ok_or(LoaderError::NoEntrySymbol)?;
    // 可选退出入口：镜像里没有该符号是正常情况（不是每个组件都需要 exit）。
    let exit_offset = function_symbol_offset(&object, symbol_table, b"kcomp_exit")?;

    let image_memory = memory::alloc_region(image_size).map_err(|_| LoaderError::OutOfMemory)?;
    let base = image_memory.region().base;
    let image = unsafe { core::slice::from_raw_parts_mut(base as *mut u8, image_size) };
    for &(index, put) in &seg_place {
        let section = object.section(index)?;
        let end = put
            .checked_add(section.size)
            .ok_or(LoaderError::UnsupportedFormat)?;
        let dst = image
            .get_mut(put..end)
            .ok_or(LoaderError::UnsupportedFormat)?;
        if section.is_nobits() {
            // BSS：无文件数据，放段 = 零填充（组件的静态 mutable 就住这里）。
            dst.fill(0);
        } else {
            let data = object.section_data(index)?;
            dst.copy_from_slice(data);
        }
    }

    apply_relocations(&object, base, image, &seg_place, &relocations)?;

    let entry = resolve_function_address(&seg_place, base, entry_offset)?;
    let exit = match exit_offset {
        Some(offset) => Some(resolve_function_address(&seg_place, base, offset)?),
        None => None,
    };
    Ok(LoadedComponent {
        base,
        entry,
        exit,
        text_size: image_size,
        memory: Some(image_memory),
    })
}

/// 在符号表里查找名为 `name` 的 `STT_FUNC`（ELF kind == 2）符号，返回
/// `(shndx, st_value)`；不存在返回 `None`。`kcomp_init` 必需，`kcomp_exit` 可选。
fn function_symbol_offset(
    object: &ElfObject<'_>,
    symbol_table: usize,
    name: &[u8],
) -> Result<Option<(usize, usize)>, LoaderError> {
    for index in 0..object.symbol_count(symbol_table)? {
        let symbol = object.symbol(symbol_table, index)?;
        if symbol.kind != 2 {
            continue;
        }
        if object.symbol_name(symbol_table, symbol)? == name {
            let value =
                usize::try_from(symbol.value).map_err(|_| LoaderError::UnsupportedFormat)?;
            return Ok(Some((symbol.shndx, value)));
        }
    }
    Ok(None)
}

/// `(shndx, st_value)` → 加载后的绝对地址（段放置偏移 + base）。
fn resolve_function_address(
    seg_place: &[(usize, usize)],
    base: usize,
    (section, value): (usize, usize),
) -> Result<usize, LoaderError> {
    let image_offset = seg_place
        .iter()
        .find(|(index, _)| *index == section)
        .map(|(_, offset)| *offset)
        .ok_or(LoaderError::NoEntrySymbol)?;
    base.checked_add(image_offset)
        .and_then(|address| address.checked_add(value))
        .ok_or(LoaderError::UnsupportedFormat)
}

/// 顶部对齐：只支持 `sh_addralign` ∈ {0,1,2,4,8}（已加载段实测上限 8）；
/// 更大的值显式失败，绝不静默按更小对齐放置。
fn align_up(value: usize, align: usize) -> Result<usize, LoaderError> {
    let mask = match align {
        0 | 1 => return Ok(value),
        2 => 1,
        4 => 3,
        8 => 7,
        _ => return Err(LoaderError::UnsupportedFormat),
    };
    value
        .checked_add(mask)
        .map(|aligned| aligned & !mask)
        .ok_or(LoaderError::UnsupportedFormat)
}

/// 依序放置全部 ALLOC 段，每段起点满足自身 `sh_addralign`。
/// 返回 `(image_size, [(shndx, put_offset)])`。
fn place_alloc_sections(sections: &[Section]) -> Result<(usize, Vec<(usize, usize)>), LoaderError> {
    let mut put = 0usize;
    let mut seg_place = Vec::new();
    for (index, section) in sections.iter().enumerate() {
        if !section.is_alloc_content() {
            continue;
        }
        put = align_up(put, section.align)?;
        seg_place.push((index, put));
        put = put
            .checked_add(section.size)
            .ok_or(LoaderError::UnsupportedFormat)?;
    }
    Ok((put, seg_place))
}

fn apply_relocations(
    object: &ElfObject<'_>,
    base: usize,
    image: &mut [u8],
    seg_place: &[(usize, usize)],
    relocations: &[ElfRelocation],
) -> Result<(), LoaderError> {
    let width = match object.class() {
        ElfClass::Bits32 => WordSize::Bits32,
        ElfClass::Bits64 => WordSize::Bits64,
    };
    let mut relocator = ComponentRelocationImpl::new();

    for &relocation in relocations {
        if ComponentRelocationImpl::is_noop(relocation.kind) {
            continue;
        }
        let symbol = object.symbol(relocation.symbol_table, relocation.symbol_index)?;
        let symbol_value =
            usize::try_from(symbol.value).map_err(|_| LoaderError::UnsupportedFormat)?;
        let symbol_address = if symbol.shndx == 0 {
            let name = object.symbol_name(relocation.symbol_table, symbol)?;
            let address =
                crate::component::export::resolve(name).ok_or(LoaderError::UnresolvedSymbol)?;
            ComponentRelocationImpl::normalize_symbol_address(address)
        } else {
            let section_offset = seg_place
                .iter()
                .find(|(index, _)| *index == symbol.shndx)
                .map(|(_, offset)| *offset)
                .ok_or(LoaderError::UnsupportedFormat)?;
            base.checked_add(section_offset)
                .and_then(|address| address.checked_add(symbol_value))
                .ok_or(LoaderError::UnsupportedFormat)?
        };
        let target = object.section(relocation.target_section)?;
        let relocation = Relocation {
            target_section: relocation.target_section,
            section_offset: relocation.offset,
            section_size: target.size,
            image_offset: seg_place
                .iter()
                .find(|(index, _)| *index == relocation.target_section)
                .and_then(|(_, offset)| offset.checked_add(relocation.offset))
                .ok_or(LoaderError::UnsupportedFormat)?,
            kind: relocation.kind,
            addend: relocation.addend,
            symbol_section: symbol.shndx,
            symbol_value,
        };
        relocator
            .apply(width, image, base, relocation, symbol_address)
            .map_err(map_relocation_error)?;
    }
    Ok(())
}

fn map_relocation_error(error: RelocationError) -> LoaderError {
    match error {
        RelocationError::Unsupported | RelocationError::AddressOverflow => {
            LoaderError::UnsupportedRelocation
        }
        RelocationError::OutOfBounds => LoaderError::UnsupportedFormat,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CORETEST_KCOMP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/core_test.kcomp"));
    const SMOKE_KCOMP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/kcomp_smoke.kcomp"));
    const SMOKE_MIN_KCOMP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/smoke_min.kcomp"));

    #[test]
    fn parses_header_of_core_test() {
        let object = ElfObject::parse(CORETEST_KCOMP).expect("parse header");
        assert!(object.sections().len() >= 4);
    }

    #[test]
    fn loads_core_test_component() {
        let _g = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let comp = load_component(CORETEST_KCOMP).expect("load core_test.kcomp");
        assert!(comp.entry >= comp.base, "kcomp_init 必须位于放置段映射内");
        assert!(comp.text_size >= 4);
    }

    #[test]
    fn rejects_wrong_machine() {
        let mut patched = CORETEST_KCOMP.to_vec();
        patched[18..20].copy_from_slice(&0x3Eu16.to_le_bytes());
        assert_eq!(load_component(&patched), Err(LoaderError::MachineMismatch));
    }

    #[test]
    fn rejects_bad_magic() {
        assert_eq!(load_component(b"not an elf"), Err(LoaderError::BadMagic));
    }

    #[test]
    fn loads_smoke_with_relocations() {
        let _g = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let comp = load_component(SMOKE_KCOMP).expect("load smoke.kcomp");
        assert!(comp.entry >= comp.base);
        assert!(comp.text_size > 0);
    }

    #[test]
    fn records_optional_exit_entry_when_symbol_present() {
        // Given：kcomp_smoke 定义 kcomp_exit（module_exit 类比）。
        let _g = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();

        // When：加载组件。
        let comp = load_component(SMOKE_KCOMP).expect("load smoke.kcomp");

        // Then：exit seam 记录到非 None 的入口地址，且与 kcomp_init 不同。
        let exit = comp.exit.expect("kcomp_smoke exports kcomp_exit");
        assert!(exit >= comp.base, "kcomp_exit 必须位于放置段映射内");
        assert_ne!(exit, comp.entry, "exit 与 init 是两个不同入口");
    }

    #[test]
    fn exit_entry_is_none_when_symbol_absent() {
        // Given：kcomp_min 只导出 kcomp_init，刻意没有 kcomp_exit。
        let _g = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();

        // When：加载组件。
        let comp = load_component(SMOKE_MIN_KCOMP).expect("load smoke_min.kcomp");

        // Then：可选 exit seam 保持 None（组件不导出 exit 是正常情况）。
        assert_eq!(comp.exit, None);
    }

    #[test]
    fn rejects_unknown_symbol() {
        let mut patched = SMOKE_KCOMP.to_vec();
        let name = b"kcore_console_write_byte";
        let pos = patched
            .windows(name.len())
            .position(|window| window == name)
            .expect("export name present in strtab");
        patched[pos] = b'x';
        assert_eq!(load_component(&patched), Err(LoaderError::UnresolvedSymbol));
    }

    #[test]
    fn rejects_component_to_component_flat_symbol() {
        // 组件→组件 依赖禁止走 flat ELF symbol namespace（interface.rs 定案）：
        // 即使符号名存在（core 侧有同名接口），Core 的 flat resolver 也只认
        // `kcore_*` 白名单，其他未定义符号一律 UnresolvedSymbol。
        let mut patched = SMOKE_KCOMP.to_vec();
        let name = b"kcore_console_write_byte";
        let pos = patched
            .windows(name.len())
            .position(|window| window == name)
            .expect("export name present in strtab");
        // 把 kcore_ 前缀改成"组件风格"的名字（如 provider 导出的符号）
        patched[pos..pos + 5].copy_from_slice(b"prov_");
        assert_eq!(load_component(&patched), Err(LoaderError::UnresolvedSymbol));
    }

    #[test]
    fn relocation_writes_correct_call_target() {
        let _g = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let comp = load_component(SMOKE_MIN_KCOMP).expect("load smoke_min.kcomp");
        let a = unsafe { core::ptr::read((comp.base + 8) as *const u32) };
        let j = unsafe { core::ptr::read((comp.base + 12) as *const u32) };
        assert_eq!(a & 0x7F, 0x17, "auipc opcode");
        assert_eq!(j & 0x7F, 0x67, "jalr opcode");
        assert_eq!((j >> 15) & 0x1F, 1, "jalr rs1=ra");
        assert_eq!((j >> 7) & 0x1F, 1, "jalr rd=ra");
        let mut imm20 = ((a >> 12) & 0xFFFFF) as i64;
        if imm20 >= 1 << 19 {
            imm20 -= 1 << 20;
        }
        let mut imm12 = ((j >> 20) & 0xFFF) as i64;
        if imm12 >= 1 << 11 {
            imm12 -= 1 << 12;
        }
        let target = (comp.base as i64 + 8) + (imm20 << 12) + imm12;
        let expected =
            crate::component::export::resolve(b"kcore_console_write_byte").unwrap() as i64;
        assert_eq!(target, expected, "重定位写入的目标必须等于 resolve 地址");
    }

    // -- 段放置对齐 ---------------------------------------------------------

    const PROGBITS: u32 = 1;
    const ALLOC: u64 = 0x2;

    fn alloc_section(size: usize, align: usize) -> Section {
        Section {
            ty: PROGBITS,
            offset: 0,
            size,
            link: 0,
            flags: ALLOC,
            info: 0,
            align,
        }
    }

    #[test]
    fn align_up_supports_1_2_4_8_and_rejects_larger() {
        assert_eq!(align_up(0, 1), Ok(0));
        assert_eq!(align_up(3, 1), Ok(3));
        assert_eq!(align_up(3, 2), Ok(4));
        assert_eq!(align_up(3, 4), Ok(4));
        assert_eq!(align_up(3, 8), Ok(8));
        assert_eq!(align_up(8, 8), Ok(8));
        assert_eq!(align_up(9, 8), Ok(16));
        assert_eq!(align_up(0, 0), Ok(0), "align 0 等同不对齐");
        assert_eq!(align_up(0, 16), Err(LoaderError::UnsupportedFormat));
        assert_eq!(align_up(0, 3), Err(LoaderError::UnsupportedFormat));
    }

    #[test]
    fn alloc_sections_are_placed_at_their_sh_addralign() {
        let sections = [
            alloc_section(3, 1),
            alloc_section(5, 8),
            alloc_section(2, 4),
            alloc_section(1, 2),
        ];
        let (image_size, place) = place_alloc_sections(&sections).expect("place sections");
        assert_eq!(place[0], (0, 0));
        assert_eq!(place[1], (1, 8), "align 8：3 字节后顶到 8");
        assert_eq!(place[2], (2, 16), "align 4：8+5=13 顶到 16");
        assert_eq!(place[3], (3, 18), "align 2：16+2=18 已对齐");
        assert_eq!(image_size, 19);
        for (index, offset) in place {
            assert_eq!(
                offset % sections[index].align,
                0,
                "shndx {index} 起点必须满足自身 sh_addralign"
            );
        }
    }

    #[test]
    fn non_alloc_sections_are_not_placed() {
        let mut metadata = alloc_section(4, 8);
        metadata.flags = 0; // PROGBITS 但非 ALLOC（如 .comment）
        let sections = [metadata, alloc_section(4, 4)];
        let (image_size, place) = place_alloc_sections(&sections).expect("place sections");
        assert_eq!(place.len(), 1);
        assert_eq!(place[0], (1, 0));
        assert_eq!(image_size, 4);
    }

    /// 性能基线（`make bench`）：**component loader 分阶段成本**。
    ///
    /// 不要只报一个 "load = 3ms"：解析 / 重定位 / 放置·拷贝 / 入口解析各自多少？
    /// - `elf_parse` / `relocations` 不分配组件镜像，可以高频跑；
    /// - `load_component` 每次都会 `alloc_region`（放置段镜像），必须把 lease
    ///   还回去，否则会耗尽测试堆 —— 因此它的数字里**含一次 region 释放**。
    ///
    /// 还缺 `kcomp_init` 执行与 `registry.declare/resolve`（要全局 registry），
    /// 与 target 侧一起做（见 docs/benchmark.md §6）。
    #[test]
    #[ignore = "性能基线：make bench 手动跑"]
    fn bench_component_load_phases() {
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();

        crate::bench::report_environment();

        // 解析：ET_REL header + section table，不碰 VM。
        crate::bench::run("loader.elf_parse", 1_000, || {
            ElfObject::parse(CORETEST_KCOMP).unwrap()
        })
        .report();

        // 只收集重定位表（不做放置/写回）。
        let object = ElfObject::parse(CORETEST_KCOMP).expect("parse");
        crate::bench::run("loader.relocations", 1_000, || {
            object.relocations().unwrap()
        })
        .report();

        // 完整加载：parse + place + alloc + copy + relocate + 解析 entry/exit。
        let mut minimal = crate::bench::Bench::new("loader.load_component.min");
        minimal.run(100, || {
            let mut comp = load_component(SMOKE_MIN_KCOMP).unwrap();
            if let Some(lease) = comp.take_memory() {
                crate::memory::free_region(lease).unwrap();
            }
            comp.entry
        });
        minimal.finish().report();

        let mut full = crate::bench::Bench::new("loader.load_component.core_test");
        full.run(100, || {
            let mut comp = load_component(CORETEST_KCOMP).unwrap();
            if let Some(lease) = comp.take_memory() {
                crate::memory::free_region(lease).unwrap();
            }
            comp.text_size
        });
        full.finish().report();
    }
}
