//! テスト用:小さな ELF をその場で組み立てる。

use std::vec::Vec;

/// テスト用の小さな ELF を組み立てる。`segs` は (種類, フラグ, vaddr, ファイル上のデータ, memsz)。
pub fn build(machine: u16, segs: &[(u32, u32, u64, &[u8], u64)]) -> Vec<u8> {
    build_typed(machine, 2, 0x40_1000, segs)
}

/// 種類(2 = ET_EXEC、3 = ET_DYN)と入口を指定して組み立てる。
pub fn build_typed(
    machine: u16,
    etype: u16,
    entry: u64,
    segs: &[(u32, u32, u64, &[u8], u64)],
) -> Vec<u8> {
    let phoff = 64u64;
    let data_start = phoff + 56 * segs.len() as u64;
    let mut out = Vec::new();
    out.extend_from_slice(b"\x7fELF");
    out.extend_from_slice(&[2, 1, 1, 0]); // 64 ビット、リトルエンディアン、版 1
    out.extend_from_slice(&[0; 8]);
    out.extend_from_slice(&etype.to_le_bytes());
    out.extend_from_slice(&machine.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&entry.to_le_bytes());
    out.extend_from_slice(&phoff.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes()); // shoff
    out.extend_from_slice(&0u32.to_le_bytes()); // flags
    out.extend_from_slice(&64u16.to_le_bytes()); // ehsize
    out.extend_from_slice(&56u16.to_le_bytes()); // phentsize
    out.extend_from_slice(&(segs.len() as u16).to_le_bytes());
    out.extend_from_slice(&[0; 6]); // shentsize, shnum, shstrndx
    let mut offset = data_start;
    for (kind, flags, vaddr, data, memsz) in segs {
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&vaddr.to_le_bytes());
        out.extend_from_slice(&vaddr.to_le_bytes()); // paddr
        out.extend_from_slice(&(data.len() as u64).to_le_bytes());
        out.extend_from_slice(&memsz.to_le_bytes());
        out.extend_from_slice(&0x1000u64.to_le_bytes());
        offset += data.len() as u64;
    }
    for (_, _, _, data, _) in segs {
        out.extend_from_slice(data);
    }
    out
}
