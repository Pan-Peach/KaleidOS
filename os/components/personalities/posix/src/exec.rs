//! Static RV64 ELF and Linux initial stack; never the Core ET_REL loader.
use crate::usermem::UserAddress;
use alloc::{vec, vec::Vec};
use kcomp_sdk::vfs::VfsPath;
use kcomp_sdk::{Errno, abi};

pub const STACK_BASE: u64 = 0x3fef_0000;
pub const STACK_SIZE: usize = 65536;

pub struct ExecRequest<'a> {
    pub executable: VfsPath,
    pub argv: &'a [&'a [u8]],
    pub envp: &'a [&'a [u8]],
}
#[derive(Debug, Clone, Default)]
pub struct LoadSegment {
    pub file_offset: u64,
    pub file_size: u64,
    pub memory_size: u64,
    pub user_address: UserAddress,
    pub readable: bool,
    pub writable: bool,
    pub executable: bool,
}
#[derive(Debug)]
pub struct LoadPlan {
    pub entry: UserAddress,
    pub segment_count: usize,
    pub phdr: u64,
    pub phnum: u16,
    pub heap: u64,
}
pub struct ImageLoader;
fn u16_at(image: &[u8], at: usize) -> Result<u16, Errno> {
    Ok(u16::from_le_bytes(
        image
            .get(at..at + 2)
            .ok_or(Errno::ENOEXEC)?
            .try_into()
            .unwrap(),
    ))
}
fn u32_at(image: &[u8], at: usize) -> Result<u32, Errno> {
    Ok(u32::from_le_bytes(
        image
            .get(at..at + 4)
            .ok_or(Errno::ENOEXEC)?
            .try_into()
            .unwrap(),
    ))
}
fn u64_at(image: &[u8], at: usize) -> Result<u64, Errno> {
    Ok(u64::from_le_bytes(
        image
            .get(at..at + 8)
            .ok_or(Errno::ENOEXEC)?
            .try_into()
            .unwrap(),
    ))
}
fn page_end(value: u64) -> Result<u64, Errno> {
    value
        .checked_add(4095)
        .map(|n| n & !4095)
        .ok_or(Errno::ENOEXEC)
}
pub fn check(status: i32) -> Result<(), Errno> {
    if status == 0 {
        Ok(())
    } else {
        Err(Errno::from_code(status))
    }
}
impl ImageLoader {
    pub fn inspect(&self, image: &[u8], segments: &mut [LoadSegment]) -> crate::Result<LoadPlan> {
        self.inspect_elf(image, segments)
            .map_err(|_| crate::Error::BadExecutable)
    }
    pub fn inspect_elf(
        &self,
        image: &[u8],
        segments: &mut [LoadSegment],
    ) -> Result<LoadPlan, Errno> {
        if image.get(..7) != Some(b"\x7fELF\x02\x01\x01")
            || u16_at(image, 16)? != 2
            || u16_at(image, 18)? != 243
            || u32_at(image, 20)? != 1
            || u16_at(image, 52)? != 64
            || u16_at(image, 54)? != 56
        {
            return Err(Errno::ENOEXEC);
        }
        let entry = u64_at(image, 24)?;
        let phoff = usize::try_from(u64_at(image, 32)?).map_err(|_| Errno::ENOEXEC)?;
        let phnum = u16_at(image, 56)?;
        if phnum == 0
            || phnum > 128
            || phoff
                .checked_add(phnum as usize * 56)
                .is_none_or(|end| end > image.len())
        {
            return Err(Errno::ENOEXEC);
        }
        let (mut count, mut phdr, mut heap) = (0, 0, 0);
        for i in 0..phnum as usize {
            let h = phoff + i * 56;
            let kind = u32_at(image, h)?;
            let flags = u32_at(image, h + 4)?;
            if kind == 2 || kind == 3 || (kind == 0x6474e551 && flags & 1 != 0) {
                return Err(Errno::ENOEXEC);
            }
            if kind != 1 {
                continue;
            }
            let offset = u64_at(image, h + 8)?;
            let address = u64_at(image, h + 16)?;
            let filesz = u64_at(image, h + 32)?;
            let memsz = u64_at(image, h + 40)?;
            let align = u64_at(image, h + 48)?;
            if filesz > memsz
                || offset
                    .checked_add(filesz)
                    .is_none_or(|end| end > image.len() as u64)
                || address < 4096
                || address
                    .checked_add(memsz)
                    .is_none_or(|end| end > STACK_BASE)
                || flags & !7 != 0
                || flags & 4 == 0
                || flags & 3 == 3
                || (align > 1 && (!align.is_power_of_two() || address % align != offset % align))
            {
                return Err(Errno::ENOEXEC);
            }
            if memsz == 0 {
                continue;
            }
            let start = address & !4095;
            let end = page_end(address + memsz)?;
            if segments[..count].iter().any(|other| {
                start < page_end(other.user_address.0 + other.memory_size).unwrap()
                    && (other.user_address.0 & !4095) < end
            }) {
                return Err(Errno::ENOEXEC);
            }
            if count >= segments.len() {
                return Err(Errno::E2BIG);
            }
            if offset <= phoff as u64 && offset + filesz >= phoff as u64 + phnum as u64 * 56 {
                phdr = address + phoff as u64 - offset;
            }
            segments[count] = LoadSegment {
                file_offset: offset,
                file_size: filesz,
                memory_size: memsz,
                user_address: UserAddress(address),
                readable: true,
                writable: flags & 2 != 0,
                executable: flags & 1 != 0,
            };
            count += 1;
            heap = heap.max(end);
        }
        if count == 0
            || !segments[..count].iter().any(|s| {
                s.executable
                    && s.user_address.0 <= entry
                    && entry < s.user_address.0 + s.memory_size
            })
        {
            return Err(Errno::ENOEXEC);
        }
        Ok(LoadPlan {
            entry: UserAddress(entry),
            segment_count: count,
            phdr,
            phnum,
            heap,
        })
    }
    pub fn prepare(&self, _request: &ExecRequest<'_>) -> crate::Result<LoadPlan> {
        Err(crate::Error::Unsupported) // General pathname execution waits for VFS.
    }
    pub fn load(
        &self,
        task: u32,
        image: &[u8],
        argv: &[Vec<u8>],
        envp: &[Vec<u8>],
        random: [u8; 16],
    ) -> Result<u64, Errno> {
        let mut segments = vec![LoadSegment::default(); 32];
        let plan = self.inspect_elf(image, &mut segments)?;
        for s in &segments[..plan.segment_count] {
            let base = s.user_address.0 & !4095;
            let len = page_end(s.user_address.0 + s.memory_size)? - base;
            let flags = 1 | if s.writable { 2 } else { 0 } | if s.executable { 4 } else { 0 };
            check(unsafe { abi::kcore_user_map(task, base, len, flags) })?;
            check(unsafe {
                abi::kcore_user_load(
                    task,
                    s.user_address.0,
                    image[s.file_offset as usize..].as_ptr(),
                    s.file_size as usize,
                )
            })?;
        }
        let (stack, sp) = initial_stack(&plan, argv, envp, random)?;
        check(unsafe { abi::kcore_user_map(task, STACK_BASE, STACK_SIZE as u64, 3) })?;
        check(unsafe { abi::kcore_user_load(task, STACK_BASE, stack.as_ptr(), stack.len()) })?;
        check(unsafe { abi::kcore_user_prepare(task, plan.entry.0, sp) })?;
        Ok(plan.heap)
    }
}
pub fn initial_stack(
    plan: &LoadPlan,
    argv: &[Vec<u8>],
    envp: &[Vec<u8>],
    random: [u8; 16],
) -> Result<(Vec<u8>, u64), Errno> {
    if argv.is_empty() || argv.len() + envp.len() > 128 {
        return Err(Errno::E2BIG);
    }
    let mut stack = vec![0; STACK_SIZE];
    let mut top = stack.len();
    let mut put = |data: &[u8], nul: bool| -> Result<u64, Errno> {
        if data.len() > 4096 || (nul && data.contains(&0)) {
            return Err(Errno::E2BIG);
        }
        top = top
            .checked_sub(data.len() + usize::from(nul))
            .ok_or(Errno::E2BIG)?;
        stack[top..top + data.len()].copy_from_slice(data);
        Ok(STACK_BASE + top as u64)
    };
    let random_at = put(&random, false)?;
    let mut arg_addresses = Vec::new();
    let mut env_addresses = Vec::new();
    for arg in argv {
        arg_addresses.push(put(arg, true)?);
    }
    for env in envp {
        env_addresses.push(put(env, true)?);
    }
    let mut words = vec![argv.len() as u64];
    words.extend_from_slice(&arg_addresses);
    words.push(0);
    words.extend_from_slice(&env_addresses);
    words.push(0);
    for (key, value) in [
        (3, plan.phdr),
        (4, 56),
        (5, plan.phnum as u64),
        (6, 4096),
        (7, 0),
        (9, plan.entry.0),
        (11, 0),
        (12, 0),
        (13, 0),
        (14, 0),
        (23, 0),
        (25, random_at),
        (31, arg_addresses[0]),
        (0, 0),
    ] {
        words.push(key);
        words.push(value);
    }
    top = top.checked_sub(words.len() * 8).ok_or(Errno::E2BIG)? & !15;
    for (i, value) in words.into_iter().enumerate() {
        stack[top + i * 8..top + (i + 1) * 8].copy_from_slice(&value.to_le_bytes());
    }
    Ok((stack, STACK_BASE + top as u64))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Vec<u8> {
        let mut image = vec![0; 4097];
        image[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        image[16..18].copy_from_slice(&2u16.to_le_bytes());
        image[18..20].copy_from_slice(&243u16.to_le_bytes());
        image[20..24].copy_from_slice(&1u32.to_le_bytes());
        image[24..32].copy_from_slice(&0x10100u64.to_le_bytes());
        image[32..40].copy_from_slice(&64u64.to_le_bytes());
        for (at, value) in [(52, 64u16), (54, 56), (56, 2)] {
            image[at..at + 2].copy_from_slice(&value.to_le_bytes());
        }
        for (at, flags, offset, base, filesz, memsz) in [
            (64, 5u32, 0u64, 0x10000u64, 4096u64, 4096u64),
            (120, 6, 4096, 0x11000, 1, 8192),
        ] {
            image[at..at + 4].copy_from_slice(&1u32.to_le_bytes());
            image[at + 4..at + 8].copy_from_slice(&flags.to_le_bytes());
            for (field, value) in [
                (8, offset),
                (16, base),
                (32, filesz),
                (40, memsz),
                (48, 4096),
            ] {
                image[at + field..at + field + 8].copy_from_slice(&value.to_le_bytes());
            }
        }
        image
    }
    #[test]
    fn elf_preserves_bss_permissions_and_headers() {
        let mut segments = vec![LoadSegment::default(); 8];
        let plan = ImageLoader.inspect_elf(&fixture(), &mut segments).unwrap();
        assert_eq!(
            (plan.phdr, plan.heap, plan.segment_count),
            (0x10040, 0x13000, 2)
        );
        assert!(segments[0].executable && !segments[0].writable);
        assert_eq!(segments[1].memory_size, 8192);
    }
    #[test]
    fn rejects_interp_wx_wrong_isa_and_overflow() {
        for (offset, value) in [(64, 3u32), (68, 7), (18, 62), (152, u32::MAX)] {
            let mut image = fixture();
            image[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            assert!(
                ImageLoader
                    .inspect_elf(&image, &mut vec![LoadSegment::default(); 8])
                    .is_err()
            );
        }
        for len in [0, 7, 63, 119, 175] {
            assert!(ImageLoader.inspect_elf(&fixture()[..len], &mut []).is_err());
        }
    }
    #[test]
    fn stack_preserves_argv_envp_and_alignment() {
        let plan = ImageLoader
            .inspect_elf(&fixture(), &mut vec![LoadSegment::default(); 8])
            .unwrap();
        let (stack, sp) = initial_stack(
            &plan,
            &[b"/app".to_vec(), b"probe".to_vec()],
            &[b"KEY=value".to_vec()],
            [42; 16],
        )
        .unwrap();
        assert_eq!(sp % 16, 0);
        let at = (sp - STACK_BASE) as usize;
        assert_eq!(u64_at(&stack, at).unwrap(), 2);
        let pointer = u64_at(&stack, at + 16).unwrap();
        assert_eq!(&stack[(pointer - STACK_BASE) as usize..][..6], b"probe\0");
        assert_eq!(u64_at(&stack, at + 24).unwrap(), 0);
        assert_eq!(u64_at(&stack, at + 40).unwrap(), 0);
    }
}
