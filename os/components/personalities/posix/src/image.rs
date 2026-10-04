//! Immutable execution-profile images, not a fake general filesystem.
use alloc::vec::Vec;
use kcomp_sdk::Errno;

pub struct Image {
    pub name: Vec<u8>,
    pub bytes: Vec<u8>,
}
pub struct Profile {
    pub images: Vec<Image>,
    pub argv: Vec<Vec<u8>>,
    pub envp: Vec<Vec<u8>>,
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], Errno> {
        let end = self.offset.checked_add(len).ok_or(Errno::EINVAL)?;
        let result = self.bytes.get(self.offset..end).ok_or(Errno::EINVAL)?;
        self.offset = end;
        Ok(result)
    }
    fn number(&mut self) -> Result<usize, Errno> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()) as usize)
    }
    fn string(&mut self) -> Result<Vec<u8>, Errno> {
        let len = self.number()?;
        if len > 4096 {
            return Err(Errno::E2BIG);
        }
        let bytes = self.take(len)?;
        if bytes.contains(&0) {
            return Err(Errno::EINVAL);
        }
        Ok(bytes.to_vec())
    }
}

pub fn decode(bytes: &[u8]) -> Result<Profile, Errno> {
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(Errno::E2BIG);
    }
    let mut reader = Reader { bytes, offset: 0 };
    let count = reader.number()?;
    let argc = reader.number()?;
    let envc = reader.number()?;
    if count == 0 || count > 8 || argc == 0 || argc + envc > 128 || reader.number()? != 0 {
        return Err(Errno::EINVAL);
    }
    let mut images: Vec<Image> = Vec::new();
    for _ in 0..count {
        let namelen = reader.number()?;
        let imagelen = reader.number()?;
        if namelen == 0 || namelen > 255 || imagelen == 0 {
            return Err(Errno::EINVAL);
        }
        let name = reader.take(namelen)?.to_vec();
        if name[0] != b'/' || name.contains(&0) || images.iter().any(|image| image.name == name) {
            return Err(Errno::EINVAL);
        }
        let bytes = reader.take(imagelen)?.to_vec();
        images.push(Image { name, bytes });
    }
    let mut argv = Vec::new();
    let mut envp = Vec::new();
    for _ in 0..argc {
        argv.push(reader.string()?);
    }
    for _ in 0..envc {
        envp.push(reader.string()?);
    }
    if reader.offset != bytes.len() {
        return Err(Errno::EINVAL);
    }
    Ok(Profile { images, argv, envp })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_truncation_reserved_and_duplicate_image_names() {
        let bytes = kcomp_sdk::posix::encode(
            &[(b"/app", b"ELF"), (b"/second", b"OTHER")],
            &[b"/app"],
            &[],
        )
        .unwrap();
        assert_eq!(decode(&bytes).unwrap().images.len(), 2);
        for len in 0..bytes.len() {
            assert!(decode(&bytes[..len]).is_err());
        }
        let mut bad = bytes.clone();
        bad[12] = 1;
        assert!(decode(&bad).is_err());
        let duplicate =
            kcomp_sdk::posix::encode(&[(b"/app", b"A"), (b"/app", b"B")], &[b"/app"], &[]).unwrap();
        assert!(decode(&duplicate).is_err());
    }
}
