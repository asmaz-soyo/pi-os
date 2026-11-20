//! ELF(Executable and Linkable Format)の読み取り。静的リンクの AArch64 実行ファイルだけを扱う。
//!
//! ```text
//! +----------------+  0
//! | ELF ヘッダ      |  種類、CPU、入口のアドレス、プログラムヘッダの位置
//! +----------------+  e_phoff
//! | プログラムヘッダ |  「ファイルのこの部分を、このアドレスに、この権限で置け」の表
//! +----------------+
//! | セグメントの中身 |  コード、データ
//! +----------------+
//! ```
//!
//! カーネルが見るのはプログラムヘッダだけ。セクション(.text、.data など)はリンカのためのもの。

use crate::Errno;
use hal::mmu::MapFlags;

pub const EM_AARCH64: u16 = 183;
const ET_EXEC: u16 = 2;
pub const PT_LOAD: u32 = 1;
pub const PT_INTERP: u32 = 3;
pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;
pub const PF_R: u32 = 4;

/// ユーザー空間の上限(48 ビットの下半分)
pub const USER_END: u64 = 0x0001_0000_0000_0000;

const EHDR_SIZE: usize = 64;
const PHDR_SIZE: usize = 56;

/// メモリに置く1つの区画。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment<'a> {
    pub vaddr: u64,
    /// メモリ上の大きさ。`data` より大きければ、残りは 0 で埋める(.bss)
    pub memsz: u64,
    /// ファイルから写す中身
    pub data: &'a [u8],
    pub flags: MapFlags,
}

/// 読み取った ELF。
#[derive(Debug, Clone, Copy)]
pub struct Elf<'a> {
    bytes: &'a [u8],
    phoff: usize,
    pub entry: u64,
    pub phnum: usize,
    pub phent: usize,
    /// プログラムヘッダがメモリ上に見えるアドレス(補助ベクタの AT_PHDR 用)
    pub phdr_vaddr: Option<u64>,
}

fn u16_at(b: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(off..off + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(off..off + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(off..off + 8)?.try_into().ok()?))
}

struct Phdr {
    kind: u32,
    flags: u32,
    offset: u64,
    vaddr: u64,
    filesz: u64,
    memsz: u64,
}

fn phdr_at(b: &[u8], off: usize) -> Option<Phdr> {
    Some(Phdr {
        kind: u32_at(b, off)?,
        flags: u32_at(b, off + 4)?,
        offset: u64_at(b, off + 8)?,
        vaddr: u64_at(b, off + 16)?,
        filesz: u64_at(b, off + 32)?,
        memsz: u64_at(b, off + 40)?,
    })
}

fn flags_of(pf: u32) -> MapFlags {
    MapFlags {
        write: pf & PF_W != 0,
        exec: pf & PF_X != 0,
        user: true,
        device: false,
    }
}

/// 実行ファイルを検証して読み取る。動かせないものは `ENOEXEC`。
pub fn parse(bytes: &[u8]) -> Result<Elf<'_>, Errno> {
    let bad = Errno::ENOEXEC;
    if bytes.len() < EHDR_SIZE || &bytes[0..4] != b"\x7fELF" {
        return Err(bad);
    }
    // 64 ビット、リトルエンディアン、版 1
    if bytes[4] != 2 || bytes[5] != 1 || bytes[6] != 1 {
        return Err(bad);
    }
    if u16_at(bytes, 16) != Some(ET_EXEC) || u16_at(bytes, 18) != Some(EM_AARCH64) {
        return Err(bad);
    }
    let entry = u64_at(bytes, 24).ok_or(bad)?;
    let phoff = u64_at(bytes, 32).ok_or(bad)? as usize;
    let phent = u16_at(bytes, 54).ok_or(bad)? as usize;
    let phnum = u16_at(bytes, 56).ok_or(bad)? as usize;
    if phent != PHDR_SIZE
        || phoff
            .checked_add(phent * phnum)
            .is_none_or(|end| end > bytes.len())
    {
        return Err(bad);
    }
    let mut phdr_vaddr = None;
    for i in 0..phnum {
        let ph = phdr_at(bytes, phoff + i * phent).ok_or(bad)?;
        match ph.kind {
            // 動的リンク(インタプリタが必要)はまだ扱わない
            PT_INTERP => return Err(bad),
            PT_LOAD => {
                let file_end = ph.offset.checked_add(ph.filesz).ok_or(bad)?;
                let mem_end = ph.vaddr.checked_add(ph.memsz).ok_or(bad)?;
                if file_end > bytes.len() as u64 || ph.filesz > ph.memsz || mem_end > USER_END {
                    return Err(bad);
                }
                // このセグメントがプログラムヘッダを含んでいれば、その仮想アドレスを覚えておく
                let ph_end = (phoff + phent * phnum) as u64;
                if ph.offset <= phoff as u64 && ph_end <= file_end {
                    phdr_vaddr = Some(ph.vaddr + (phoff as u64 - ph.offset));
                }
            }
            _ => {}
        }
    }
    Ok(Elf {
        bytes,
        phoff,
        entry,
        phnum,
        phent,
        phdr_vaddr,
    })
}

impl<'a> Elf<'a> {
    /// 読み込むセグメント(PT_LOAD)の一覧。
    pub fn segments(&self) -> impl Iterator<Item = Segment<'a>> + '_ {
        (0..self.phnum)
            .filter_map(|i| phdr_at(self.bytes, self.phoff + i * self.phent))
            .filter(|ph| ph.kind == PT_LOAD)
            .map(|ph| Segment {
                vaddr: ph.vaddr,
                memsz: ph.memsz,
                data: &self.bytes[ph.offset as usize..(ph.offset + ph.filesz) as usize],
                flags: flags_of(ph.flags),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Errno;
    use hal::mmu::MapFlags;
    use std::vec::Vec;

    use crate::exec::test_elf::build;

    const RX: u32 = PF_R | PF_X;
    const RW: u32 = PF_R | PF_W;

    #[test]
    fn parses_entry_and_load_segments_with_permissions() {
        let bytes = build(
            EM_AARCH64,
            &[
                (PT_LOAD, RX, 0x40_0000, b"code", 4),
                (PT_LOAD, RW, 0x41_0000, b"data", 0x2000),
                (PT_LOAD, PF_R, 0x42_0000, b"ro", 2),
            ],
        );
        let elf = parse(&bytes).unwrap();
        assert_eq!(elf.entry, 0x40_1000);
        let segs: Vec<_> = elf.segments().collect();
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].vaddr, 0x40_0000);
        assert_eq!(segs[0].data, b"code");
        assert_eq!(segs[0].flags, MapFlags::USER_RX);
        assert_eq!(segs[1].memsz, 0x2000);
        assert_eq!(segs[1].flags, MapFlags::USER_RW);
        assert_eq!(segs[2].flags, MapFlags::USER_R);
    }

    #[test]
    fn program_header_location_is_reported_for_the_aux_vector() {
        // ファイルの先頭(ヘッダ込み)を読み込む PT_LOAD があれば、AT_PHDR はその中の位置になる
        let bytes = build(EM_AARCH64, &[(PT_LOAD, RX, 0x40_0000, b"x", 1)]);
        let elf = parse(&bytes).unwrap();
        assert_eq!(elf.phnum, 1);
        assert_eq!(elf.phent, 56);
        // 最初のセグメントのファイル上の位置は 64 + 56 = 120 なので、ヘッダは含まれない
        assert_eq!(elf.phdr_vaddr, None);
    }

    #[test]
    fn rejects_non_elf_and_foreign_machines() {
        assert_eq!(parse(b"not an elf at all").err(), Some(Errno::ENOEXEC));
        let x86 = build(62, &[(PT_LOAD, RX, 0x40_0000, b"x", 1)]);
        assert_eq!(parse(&x86).err(), Some(Errno::ENOEXEC));
        let mut elf32 = build(EM_AARCH64, &[]);
        elf32[4] = 1;
        assert_eq!(parse(&elf32).err(), Some(Errno::ENOEXEC));
    }

    #[test]
    fn rejects_truncated_files() {
        let bytes = build(EM_AARCH64, &[(PT_LOAD, RX, 0x40_0000, b"code", 4)]);
        assert_eq!(parse(&bytes[..100]).err(), Some(Errno::ENOEXEC));
        assert_eq!(parse(&bytes[..bytes.len() - 1]).err(), Some(Errno::ENOEXEC));
    }

    #[test]
    fn rejects_dynamically_linked_programs() {
        let bytes = build(EM_AARCH64, &[(PT_INTERP, PF_R, 0, b"/lib/ld.so", 10)]);
        assert_eq!(parse(&bytes).err(), Some(Errno::ENOEXEC));
    }

    #[test]
    fn rejects_segments_outside_user_space() {
        let bytes = build(EM_AARCH64, &[(PT_LOAD, RW, 0xffff_0000_0000_0000, b"x", 1)]);
        assert_eq!(parse(&bytes).err(), Some(Errno::ENOEXEC));
        let bytes = build(EM_AARCH64, &[(PT_LOAD, RW, 0x40_0000, b"xx", 1)]);
        assert_eq!(parse(&bytes).err(), Some(Errno::ENOEXEC), "filesz > memsz");
    }
}
