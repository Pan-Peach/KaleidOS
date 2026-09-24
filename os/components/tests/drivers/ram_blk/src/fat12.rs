//! 合成 FAT12 卷（只读）：给 FatFs 一个**真实可挂载**的文件系统。
//!
//! 内容在编译期生成（[`IMAGE`] 是 `const fn` 的产物），运行期不可变——provider
//! 的 `read` 因此不需要锁。卷里只有一个文件 `HELLO.TXT`，内容是 [`HELLO`]：
//! QEMU runner 逐字节比对 C consumer 读到的这一行，证明"正确字节穿过了
//! Direct 调用链"。
//!
//! # 几何（最小可用 FAT12）
//!
//! ```text
//! 512 B/sector，64 sectors（32 KiB），1 sector/cluster，2 份 FAT（各 1 sector），
//! 16 个根目录项（1 sector）→ 数据区 60 簇 < 4085 ⇒ FAT12。
//! sector 0 引导扇区 / 1 FAT#1 / 2 FAT#2 / 3 根目录 / 4.. 数据（簇 2 起）。
//! ```

/// 扇区大小（字节）——必须与 block 契约的 sector 一致。
pub const SECTOR: usize = 512;
/// 卷大小（扇区数）。
pub const SECTORS: usize = 64;
/// HELLO.TXT 的内容（QEMU 侧逐字节比对）。
pub const HELLO: &[u8] = b"KALEIDOS BLOCK CHAIN OK";

const IMAGE_BYTES: usize = SECTOR * SECTORS;
const ROOT_ENTRIES: usize = 16;
const FAT1_SECTOR: usize = 1;
const FAT2_SECTOR: usize = 2;
const ROOT_DIR_SECTOR: usize = 3;
/// FAT12 的数据簇号从 2 起：本卷第一个（也是唯一一个）文件簇。
const HELLO_CLUSTER: u16 = 2;
const HELLO_SECTOR: usize = 4;

/// 卷内容（编译期生成、只读）。
pub static IMAGE: [u8; IMAGE_BYTES] = build();

const fn put_u16(image: &mut [u8; IMAGE_BYTES], offset: usize, value: u16) {
    image[offset] = value as u8;
    image[offset + 1] = (value >> 8) as u8;
}

const fn put_u32(image: &mut [u8; IMAGE_BYTES], offset: usize, value: u32) {
    image[offset] = value as u8;
    image[offset + 1] = (value >> 8) as u8;
    image[offset + 2] = (value >> 16) as u8;
    image[offset + 3] = (value >> 24) as u8;
}

const fn put_bytes(image: &mut [u8; IMAGE_BYTES], offset: usize, bytes: &[u8]) {
    let mut i = 0;
    while i < bytes.len() {
        image[offset + i] = bytes[i];
        i += 1;
    }
}

/// 引导扇区（BPB）：FAT12 判定、FAT 布局、根目录项数都在这里。
const fn write_boot_sector(image: &mut [u8; IMAGE_BYTES]) {
    put_bytes(image, 0, &[0xEB, 0x3C, 0x90]); // jmp + nop
    put_bytes(image, 3, b"MSDOS5.0"); // OEM
    put_u16(image, 11, SECTOR as u16); // 每扇区字节数
    image[13] = 1; // 每簇扇区数
    put_u16(image, 14, 1); // 保留扇区数
    image[16] = 2; // FAT 份数
    put_u16(image, 17, ROOT_ENTRIES as u16); // 根目录项数
    put_u16(image, 19, SECTORS as u16); // 总扇区数（16 位）
    image[21] = 0xF8; // media descriptor（固定盘）
    put_u16(image, 22, 1); // 每 FAT 扇区数
    put_u16(image, 24, 8); // 每道扇区数（几何，FatFs 不使用）
    put_u16(image, 26, 1); // 磁头数（几何）
    image[36] = 0x00; // 驱动器号
    image[38] = 0x29; // 扩展引导签名
    put_bytes(image, 39, &[0x12, 0x34, 0x56, 0x78]); // 卷 id
    put_bytes(image, 43, b"KALEIDOS   "); // 卷标（11 字节）
    put_bytes(image, 54, b"FAT12   "); // 文件系统类型（8 字节）
    image[510] = 0x55;
    image[511] = 0xAA;
}

/// FAT12 前三个表项：`FAT[0] = 0xFF8`（media）、`FAT[1] = 0xFFF`（EOC）、
/// `FAT[2] = 0xFFF`（HELLO.TXT 占一个簇）。12 位打包：`[F8 FF FF FF 0F]`。
const fn write_fat(image: &mut [u8; IMAGE_BYTES], sector: usize) {
    put_bytes(image, sector * SECTOR, &[0xF8, 0xFF, 0xFF, 0xFF, 0x0F]);
}

/// 根目录：一条 8.3 目录项 `HELLO.TXT`（attr = archive，首簇 2，大小 = [`HELLO`]）。
const fn write_root_dir(image: &mut [u8; IMAGE_BYTES]) {
    let entry = ROOT_DIR_SECTOR * SECTOR;
    put_bytes(image, entry, b"HELLO   TXT"); // 8+3 名字
    image[entry + 11] = 0x20; // attr = archive
    // 12..20 = ntres / 时间戳（0）；20..22 = fstClusHI（FAT12 必须为 0）。
    put_u16(image, entry + 26, HELLO_CLUSTER); // fstClusLO
    put_u32(image, entry + 28, HELLO.len() as u32); // fileSize
}

const fn build() -> [u8; IMAGE_BYTES] {
    let mut image = [0u8; IMAGE_BYTES];
    write_boot_sector(&mut image);
    write_fat(&mut image, FAT1_SECTOR);
    write_fat(&mut image, FAT2_SECTOR);
    write_root_dir(&mut image);
    put_bytes(&mut image, HELLO_SECTOR * SECTOR, HELLO);
    image
}
