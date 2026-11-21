//! ext2 ファイルシステム(読み書き)。
//!
//! ```text
//! バイト 1024〜     スーパーブロック(全体の情報:ブロックの大きさ、グループの数、…)
//! 次のブロック〜    グループ記述子の表(グループごとに、ビットマップと inode 表の場所)
//! 各グループ        ブロックのビットマップ、inode のビットマップ、inode 表、データブロック
//! ```
//!
//! ファイルやディレクトリは inode(番号で引く)で表す。inode は種類・大きさ・所有者と、
//! データブロックの場所(直接 12 個 + 1 段 / 2 段 / 3 段の間接)を持つ。ルートは inode 2。

use crate::Errno;
use crate::block::SharedDisk;
use crate::fs::vfs::{DirEntry, File, FileKind, FileSystem, Stat};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use hal::block::BLOCK_SIZE;

const MAGIC: u16 = 0xef53;
/// スーパーブロックの機能の印:ジャーナルがある(compat)、リプレイが要る(incompat)
const COMPAT_HAS_JOURNAL: u32 = 0x4;
const INCOMPAT_RECOVER: u32 = 0x4;
const ROOT_INO: u32 = 2;

struct Inner {
    disk: SharedDisk,
    block_size: u64,
    inodes_per_group: u32,
    inode_size: u64,
    /// グループごとの inode 表の最初のブロック
    inode_tables: Vec<u32>,
    blocks_count: u64,
    blocks_per_group: u64,
    first_data_block: u64,
    /// グループ記述子の表の最初のブロック
    gdt_block: u64,
    /// メタデータ(ビットマップ、記述子、ディレクトリ)を変える処理を 1 つずつにする
    meta: crate::sync::SpinLock<()>,
}

#[derive(Debug, Clone, Copy, Default)]
struct Inode {
    mode: u16,
    uid: u32,
    gid: u32,
    size: u64,
    links: u16,
    /// 使っているブロックの数(512 バイト単位、間接ブロックも含む)
    sectors: u32,
    dtime: u32,
    blocks: [u32; 15],
    /// 時刻(1970 年からの秒):最後に読んだ、属性を変えた、中身を変えた
    atime: u32,
    ctime: u32,
    mtime: u32,
}

fn secs(s: u32) -> crate::fs::vfs::Timespec {
    crate::fs::vfs::Timespec {
        sec: s as i64,
        nsec: 0,
    }
}

/// 今の時刻(秒)。inode の時刻の欄は 32 ビット
fn now_secs() -> u32 {
    crate::fs::vfs::Timespec::now().sec as u32
}

impl Inode {
    /// 作ったときの時刻を入れる
    fn born(mut self) -> Self {
        let now = now_secs();
        (self.atime, self.ctime, self.mtime) = (now, now, now);
        self
    }
}

fn le16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn le32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

impl Inner {
    /// ディスクのバイト位置 `offset` から `buf` を読む
    fn read_bytes(&self, offset: u64, buf: &mut [u8]) -> Result<(), Errno> {
        let mut disk = self.disk.lock();
        let mut sector = [0u8; BLOCK_SIZE];
        let mut done = 0;
        while done < buf.len() {
            let pos = offset + done as u64;
            let within = (pos % BLOCK_SIZE as u64) as usize;
            disk.read_block(pos / BLOCK_SIZE as u64, &mut sector)
                .map_err(|_| Errno::EIO)?;
            let n = (BLOCK_SIZE - within).min(buf.len() - done);
            buf[done..done + n].copy_from_slice(&sector[within..within + n]);
            done += n;
        }
        Ok(())
    }

    fn read_u32(&self, block: u32, index: u64) -> Result<u32, Errno> {
        let mut b = [0u8; 4];
        self.read_bytes(block as u64 * self.block_size + index * 4, &mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    fn inode(&self, ino: u32) -> Result<Inode, Errno> {
        let group = ((ino - 1) / self.inodes_per_group) as usize;
        let index = ((ino - 1) % self.inodes_per_group) as u64;
        let table = *self.inode_tables.get(group).ok_or(Errno::EIO)?;
        let mut raw = vec![0u8; self.inode_size as usize];
        self.read_bytes(
            table as u64 * self.block_size + index * self.inode_size,
            &mut raw,
        )?;
        let mut blocks = [0u32; 15];
        for (i, b) in blocks.iter_mut().enumerate() {
            *b = le32(&raw, 40 + i * 4);
        }
        Ok(Inode {
            mode: le16(&raw, 0),
            // 下の 16 ビットと、Linux の拡張の上の 16 ビット(120 バイト目、122 バイト目)
            uid: le16(&raw, 2) as u32 | (le16(&raw, 120) as u32) << 16,
            gid: le16(&raw, 24) as u32 | (le16(&raw, 122) as u32) << 16,
            size: le32(&raw, 4) as u64 | ((le32(&raw, 108) as u64) << 32),
            links: le16(&raw, 26),
            sectors: le32(&raw, 28),
            dtime: le32(&raw, 20),
            blocks,
            atime: le32(&raw, 8),
            ctime: le32(&raw, 12),
            mtime: le32(&raw, 16),
        })
    }

    /// ファイルの `index` 番目のブロックの場所(0 なら穴:0 で埋まっているとみなす)
    fn block_of(&self, inode: &Inode, index: u64) -> Result<u32, Errno> {
        let per = self.block_size / 4; // 1 つの間接ブロックに入るブロック番号の数
        if index < 12 {
            return Ok(inode.blocks[index as usize]);
        }
        let mut i = index - 12;
        if i < per {
            return self.indirect(inode.blocks[12], &[i]);
        }
        i -= per;
        if i < per * per {
            return self.indirect(inode.blocks[13], &[i / per, i % per]);
        }
        i -= per * per;
        self.indirect(inode.blocks[14], &[i / (per * per), i / per % per, i % per])
    }

    /// 間接ブロックを `path` の添字の順にたどる
    fn indirect(&self, mut block: u32, path: &[u64]) -> Result<u32, Errno> {
        for &i in path {
            if block == 0 {
                return Ok(0);
            }
            block = self.read_u32(block, i)?;
        }
        Ok(block)
    }

    fn read_file(&self, inode: &Inode, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        let len = (buf.len() as u64).min(inode.size.saturating_sub(offset)) as usize;
        let mut done = 0;
        while done < len {
            let pos = offset + done as u64;
            let within = pos % self.block_size;
            let n = ((self.block_size - within) as usize).min(len - done);
            match self.block_of(inode, pos / self.block_size)? {
                0 => buf[done..done + n].fill(0),
                block => self.read_bytes(
                    block as u64 * self.block_size + within,
                    &mut buf[done..done + n],
                )?,
            }
            done += n;
        }
        Ok(len)
    }

    /// ディレクトリの項目:(名前, inode 番号)。`.` と `..` も含む
    fn entries(&self, inode: &Inode) -> Result<Vec<(String, u32)>, Errno> {
        let mut data = vec![0u8; inode.size as usize];
        self.read_file(inode, 0, &mut data)?;
        let mut out = Vec::new();
        let mut at = 0;
        while at + 8 <= data.len() {
            let ino = le32(&data, at);
            let rec_len = le16(&data, at + 4) as usize;
            let name_len = data[at + 6] as usize;
            if rec_len == 0 {
                break;
            }
            if ino != 0 {
                let name = &data[at + 8..at + 8 + name_len];
                out.push((String::from_utf8_lossy(name).into_owned(), ino));
            }
            at += rec_len;
        }
        Ok(out)
    }

    /// ディレクトリ `dir_ino` の中の `name`
    fn lookup_in(&self, dir_ino: u32, name: &str) -> Result<(u32, Inode), Errno> {
        let dir = self.inode(dir_ino)?;
        if kind_of(dir.mode) != FileKind::Directory {
            return Err(Errno::ENOTDIR);
        }
        let ino = self
            .entries(&dir)?
            .into_iter()
            .find(|(n, _)| n == name)
            .map(|(_, i)| i)
            .ok_or(Errno::ENOENT)?;
        Ok((ino, self.inode(ino)?))
    }

    fn lookup(&self, path: &str) -> Result<(u32, Inode), Errno> {
        let mut ino = ROOT_INO;
        let mut inode = self.inode(ino)?;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            if kind_of(inode.mode) != FileKind::Directory {
                return Err(Errno::ENOTDIR);
            }
            ino = self
                .entries(&inode)?
                .into_iter()
                .find(|(name, _)| name == part)
                .map(|(_, i)| i)
                .ok_or(Errno::ENOENT)?;
            inode = self.inode(ino)?;
        }
        Ok((ino, inode))
    }
}

const S_IFREG: u16 = 0x8000;
const S_IFDIR: u16 = 0x4000;
const S_IFLNK: u16 = 0xa000;
/// これより短いリンクは、指す先を inode のブロック番号の欄(60 バイト)にそのまま入れる
const FAST_LINK_MAX: usize = 60;
const SB: u64 = 1024;

/// 書き込みの処理(ビットマップでの割り当て、inode とディレクトリの更新)
impl Inner {
    fn write_bytes(&self, offset: u64, data: &[u8]) -> Result<(), Errno> {
        let mut disk = self.disk.lock();
        let mut sector = [0u8; BLOCK_SIZE];
        let mut done = 0;
        while done < data.len() {
            let pos = offset + done as u64;
            let lba = pos / BLOCK_SIZE as u64;
            let within = (pos % BLOCK_SIZE as u64) as usize;
            let n = (BLOCK_SIZE - within).min(data.len() - done);
            if n < BLOCK_SIZE {
                disk.read_block(lba, &mut sector).map_err(|_| Errno::EIO)?;
            }
            sector[within..within + n].copy_from_slice(&data[done..done + n]);
            disk.write_block(lba, &sector).map_err(|_| Errno::EIO)?;
            done += n;
        }
        Ok(())
    }

    fn add_u32(&self, offset: u64, delta: i64) -> Result<(), Errno> {
        let mut b = [0u8; 4];
        self.read_bytes(offset, &mut b)?;
        let v = (u32::from_le_bytes(b) as i64 + delta) as u32;
        self.write_bytes(offset, &v.to_le_bytes())
    }

    fn add_u16(&self, offset: u64, delta: i64) -> Result<(), Errno> {
        let mut b = [0u8; 2];
        self.read_bytes(offset, &mut b)?;
        let v = (u16::from_le_bytes(b) as i64 + delta) as u16;
        self.write_bytes(offset, &v.to_le_bytes())
    }

    /// グループ記述子の欄の場所
    fn gd(&self, group: usize, field: u64) -> u64 {
        self.gdt_block * self.block_size + group as u64 * 32 + field
    }

    fn gd_u32(&self, group: usize, field: u64) -> Result<u32, Errno> {
        let mut b = [0u8; 4];
        self.read_bytes(self.gd(group, field), &mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    /// ビットマップのブロック `bitmap` から、`bits` 個のうち空いているビットを 1 つ取る
    fn take_bit(&self, bitmap: u32, bits: usize) -> Result<Option<usize>, Errno> {
        let mut map = vec![0u8; self.block_size as usize];
        self.read_bytes(bitmap as u64 * self.block_size, &mut map)?;
        for bit in 0..bits {
            if map[bit / 8] & (1 << (bit % 8)) == 0 {
                self.write_bytes(
                    bitmap as u64 * self.block_size + (bit / 8) as u64,
                    &[map[bit / 8] | (1 << (bit % 8))],
                )?;
                return Ok(Some(bit));
            }
        }
        Ok(None)
    }

    fn clear_bit(&self, bitmap: u32, bit: usize) -> Result<(), Errno> {
        let at = bitmap as u64 * self.block_size + (bit / 8) as u64;
        let mut b = [0u8; 1];
        self.read_bytes(at, &mut b)?;
        self.write_bytes(at, &[b[0] & !(1 << (bit % 8))])
    }

    /// 空きブロックを 1 つ取り、0 で埋める
    fn alloc_block(&self) -> Result<u32, Errno> {
        for g in 0..self.inode_tables.len() {
            let first = self.first_data_block + g as u64 * self.blocks_per_group;
            let bits = self.blocks_per_group.min(self.blocks_count - first) as usize;
            if let Some(bit) = self.take_bit(self.gd_u32(g, 0)?, bits)? {
                self.add_u16(self.gd(g, 12), -1)?;
                self.add_u32(SB + 12, -1)?;
                let block = first + bit as u64;
                self.write_bytes(
                    block * self.block_size,
                    &vec![0u8; self.block_size as usize],
                )?;
                return Ok(block as u32);
            }
        }
        Err(Errno::ENOSPC)
    }

    fn free_block(&self, block: u32) -> Result<(), Errno> {
        let rel = block as u64 - self.first_data_block;
        let g = (rel / self.blocks_per_group) as usize;
        self.clear_bit(self.gd_u32(g, 0)?, (rel % self.blocks_per_group) as usize)?;
        self.add_u16(self.gd(g, 12), 1)?;
        self.add_u32(SB + 12, 1)
    }

    fn alloc_inode(&self, dir: bool) -> Result<u32, Errno> {
        for g in 0..self.inode_tables.len() {
            if let Some(bit) = self.take_bit(self.gd_u32(g, 4)?, self.inodes_per_group as usize)? {
                self.add_u16(self.gd(g, 14), -1)?;
                self.add_u32(SB + 16, -1)?;
                if dir {
                    self.add_u16(self.gd(g, 16), 1)?;
                }
                return Ok(g as u32 * self.inodes_per_group + bit as u32 + 1);
            }
        }
        Err(Errno::ENOSPC)
    }

    fn free_inode(&self, ino: u32, dir: bool) -> Result<(), Errno> {
        let g = ((ino - 1) / self.inodes_per_group) as usize;
        self.clear_bit(
            self.gd_u32(g, 4)?,
            ((ino - 1) % self.inodes_per_group) as usize,
        )?;
        self.add_u16(self.gd(g, 14), 1)?;
        self.add_u32(SB + 16, 1)?;
        if dir {
            self.add_u16(self.gd(g, 16), -1)?;
        }
        Ok(())
    }

    fn write_inode(&self, ino: u32, inode: &Inode) -> Result<(), Errno> {
        let group = ((ino - 1) / self.inodes_per_group) as usize;
        let index = ((ino - 1) % self.inodes_per_group) as u64;
        let at = self.inode_tables[group] as u64 * self.block_size + index * self.inode_size;
        let mut raw = vec![0u8; self.inode_size as usize];
        self.read_bytes(at, &mut raw)?;
        raw[0..2].copy_from_slice(&inode.mode.to_le_bytes());
        raw[2..4].copy_from_slice(&(inode.uid as u16).to_le_bytes());
        raw[4..8].copy_from_slice(&(inode.size as u32).to_le_bytes());
        raw[8..12].copy_from_slice(&inode.atime.to_le_bytes());
        raw[12..16].copy_from_slice(&inode.ctime.to_le_bytes());
        raw[16..20].copy_from_slice(&inode.mtime.to_le_bytes());
        raw[20..24].copy_from_slice(&inode.dtime.to_le_bytes());
        raw[24..26].copy_from_slice(&(inode.gid as u16).to_le_bytes());
        raw[26..28].copy_from_slice(&inode.links.to_le_bytes());
        raw[120..122].copy_from_slice(&((inode.uid >> 16) as u16).to_le_bytes());
        raw[122..124].copy_from_slice(&((inode.gid >> 16) as u16).to_le_bytes());
        raw[28..32].copy_from_slice(&inode.sectors.to_le_bytes());
        for (i, b) in inode.blocks.iter().enumerate() {
            raw[40 + i * 4..44 + i * 4].copy_from_slice(&b.to_le_bytes());
        }
        if kind_of(inode.mode) == FileKind::Regular {
            raw[108..112].copy_from_slice(&((inode.size >> 32) as u32).to_le_bytes());
        }
        self.write_bytes(at, &raw)
    }

    /// 新しいブロックを取り、inode の使用量に数える
    fn alloc_for(&self, inode: &mut Inode) -> Result<u32, Errno> {
        let b = self.alloc_block()?;
        inode.sectors += (self.block_size / 512) as u32;
        Ok(b)
    }

    /// 間接ブロック `table` の `i` 番目を読み、0 なら新しく取って書き込む
    fn slot(&self, inode: &mut Inode, table: u32, i: u64) -> Result<u32, Errno> {
        match self.read_u32(table, i)? {
            0 => {
                let b = self.alloc_for(inode)?;
                self.write_bytes(table as u64 * self.block_size + i * 4, &b.to_le_bytes())?;
                Ok(b)
            }
            b => Ok(b),
        }
    }

    /// ファイルの `index` 番目のブロックの場所。なければ取る(1 段・2 段の間接まで)
    fn block_for_write(&self, inode: &mut Inode, index: u64) -> Result<u32, Errno> {
        let per = self.block_size / 4;
        if index < 12 {
            if inode.blocks[index as usize] == 0 {
                inode.blocks[index as usize] = self.alloc_for(inode)?;
            }
            return Ok(inode.blocks[index as usize]);
        }
        let i = index - 12;
        if i < per {
            if inode.blocks[12] == 0 {
                inode.blocks[12] = self.alloc_for(inode)?;
            }
            return self.slot(inode, inode.blocks[12], i);
        }
        let i = i - per;
        if i < per * per {
            if inode.blocks[13] == 0 {
                inode.blocks[13] = self.alloc_for(inode)?;
            }
            let mid = self.slot(inode, inode.blocks[13], i / per)?;
            return self.slot(inode, mid, i % per);
        }
        Err(Errno::EFBIG)
    }

    fn write_file(&self, inode: &mut Inode, offset: u64, data: &[u8]) -> Result<usize, Errno> {
        let mut done = 0;
        while done < data.len() {
            let pos = offset + done as u64;
            let within = pos % self.block_size;
            let n = ((self.block_size - within) as usize).min(data.len() - done);
            let block = self.block_for_write(inode, pos / self.block_size)?;
            self.write_bytes(
                block as u64 * self.block_size + within,
                &data[done..done + n],
            )?;
            done += n;
        }
        inode.size = inode.size.max(offset + data.len() as u64);
        Ok(data.len())
    }

    /// `keep` 番目以降のブロックを返す(間接ブロックも、中が空になれば返す)
    fn free_from(&self, inode: &mut Inode, keep: u64) -> Result<(), Errno> {
        let per = self.block_size / 4;
        let unit = (self.block_size / 512) as u32;
        for i in keep.min(12)..12 {
            if inode.blocks[i as usize] != 0 {
                self.free_block(inode.blocks[i as usize])?;
                inode.blocks[i as usize] = 0;
                inode.sectors -= unit;
            }
        }
        // 1 段の間接:ファイルのブロック 12 から per 個
        if inode.blocks[12] != 0 {
            let first = keep.saturating_sub(12);
            for i in first..per {
                let b = self.read_u32(inode.blocks[12], i)?;
                if b != 0 {
                    self.free_block(b)?;
                    self.write_bytes(inode.blocks[12] as u64 * self.block_size + i * 4, &[0; 4])?;
                    inode.sectors -= unit;
                }
            }
            if first == 0 {
                self.free_block(inode.blocks[12])?;
                inode.blocks[12] = 0;
                inode.sectors -= unit;
            }
        }
        // 2 段の間接:ファイルのブロック 12 + per から
        if inode.blocks[13] != 0 {
            let first = keep.saturating_sub(12 + per);
            for m in 0..per {
                let mid = self.read_u32(inode.blocks[13], m)?;
                if mid == 0 {
                    continue;
                }
                let start = first.saturating_sub(m * per).min(per);
                for i in start..per {
                    let b = self.read_u32(mid, i)?;
                    if b != 0 {
                        self.free_block(b)?;
                        self.write_bytes(mid as u64 * self.block_size + i * 4, &[0; 4])?;
                        inode.sectors -= unit;
                    }
                }
                if start == 0 {
                    self.free_block(mid)?;
                    self.write_bytes(inode.blocks[13] as u64 * self.block_size + m * 4, &[0; 4])?;
                    inode.sectors -= unit;
                }
            }
            if first == 0 {
                self.free_block(inode.blocks[13])?;
                inode.blocks[13] = 0;
                inode.sectors -= unit;
            }
        }
        Ok(())
    }

    fn truncate(&self, ino: u32, len: u64) -> Result<(), Errno> {
        let mut inode = self.inode(ino)?;
        if len < inode.size {
            self.free_from(&mut inode, len.div_ceil(self.block_size))?;
            // 残ったブロックの、len より後ろを 0 にする
            if !len.is_multiple_of(self.block_size)
                && let Ok(b) = self.block_of(&inode, len / self.block_size)
                && b != 0
            {
                let within = len % self.block_size;
                self.write_bytes(
                    b as u64 * self.block_size + within,
                    &vec![0u8; (self.block_size - within) as usize],
                )?;
            }
        }
        inode.size = len;
        self.write_inode(ino, &inode)
    }

    /// ディレクトリに項目を足す。入る隙間がなければブロックを足す
    fn add_entry(&self, dir_ino: u32, name: &str, ino: u32, kind: FileKind) -> Result<(), Errno> {
        let mut d = self.inode(dir_ino)?;
        let need = (8 + name.len()).div_ceil(4) * 4;
        // 項目の種類:1 = ファイル、2 = ディレクトリ、7 = シンボリックリンク
        let ftype = match kind {
            FileKind::Directory => 2,
            FileKind::Symlink => 7,
            _ => 1,
        };
        let bs = self.block_size as usize;
        let blocks = d.size / self.block_size;
        for index in 0..blocks {
            let block = self.block_of(&d, index)?;
            let base = block as u64 * self.block_size;
            let mut data = vec![0u8; bs];
            self.read_bytes(base, &mut data)?;
            let mut at = 0;
            while at < bs {
                let e_ino = le32(&data, at);
                let rec = le16(&data, at + 4) as usize;
                let used = if e_ino == 0 {
                    0
                } else {
                    (8 + data[at + 6] as usize).div_ceil(4) * 4
                };
                if rec - used >= need {
                    let new_at = at + used;
                    if e_ino != 0 {
                        self.write_bytes(base + at as u64 + 4, &(used as u16).to_le_bytes())?;
                    }
                    let mut e = vec![0u8; 8 + name.len()];
                    e[0..4].copy_from_slice(&ino.to_le_bytes());
                    e[4..6].copy_from_slice(&((rec - used) as u16).to_le_bytes());
                    e[6] = name.len() as u8;
                    e[7] = ftype;
                    e[8..].copy_from_slice(name.as_bytes());
                    return self.write_bytes(base + new_at as u64, &e);
                }
                at += rec;
            }
        }
        // 新しいブロックに、ブロック全体を使う項目として書く
        let block = self.block_for_write(&mut d, blocks)?;
        let mut e = vec![0u8; 8 + name.len()];
        e[0..4].copy_from_slice(&ino.to_le_bytes());
        e[4..6].copy_from_slice(&(bs as u16).to_le_bytes());
        e[6] = name.len() as u8;
        e[7] = ftype;
        e[8..].copy_from_slice(name.as_bytes());
        self.write_bytes(block as u64 * self.block_size, &e)?;
        d.size += self.block_size;
        self.write_inode(dir_ino, &d)
    }

    /// ディレクトリから項目を消し、その inode 番号を返す
    fn remove_entry(&self, dir: &Inode, name: &str) -> Result<u32, Errno> {
        let bs = self.block_size as usize;
        for index in 0..dir.size / self.block_size {
            let block = self.block_of(dir, index)?;
            let base = block as u64 * self.block_size;
            let mut data = vec![0u8; bs];
            self.read_bytes(base, &mut data)?;
            let (mut at, mut prev) = (0, None);
            while at < bs {
                let ino = le32(&data, at);
                let rec = le16(&data, at + 4) as usize;
                let len = data[at + 6] as usize;
                if ino != 0 && &data[at + 8..at + 8 + len] == name.as_bytes() {
                    match prev {
                        // 前の項目に、この項目の長さを足して飲み込ませる
                        Some(p) => {
                            let prev_rec = le16(&data, p + 4) as usize;
                            self.write_bytes(
                                base + p as u64 + 4,
                                &((prev_rec + rec) as u16).to_le_bytes(),
                            )?;
                        }
                        // ブロックの先頭なら、inode 番号を 0 にして空きにする
                        None => self.write_bytes(base + at as u64, &[0; 4])?,
                    }
                    return Ok(ino);
                }
                prev = Some(at);
                at += rec;
            }
        }
        Err(Errno::ENOENT)
    }
}

impl Inner {
    /// ディレクトリ `dir` の「..」の項目を `parent` に向ける(「..」は最初のブロックにある)
    fn set_dotdot(&self, dir: &Inode, parent: u32) -> Result<(), Errno> {
        let bs = self.block_size as usize;
        let base = self.block_of(dir, 0)? as u64 * self.block_size;
        let mut data = vec![0u8; bs];
        self.read_bytes(base, &mut data)?;
        let mut at = 0;
        while at + 8 <= bs {
            let rec = le16(&data, at + 4) as usize;
            let len = data[at + 6] as usize;
            if len == 2 && &data[at + 8..at + 10] == b".." {
                return self.write_bytes(base + at as u64, &parent.to_le_bytes());
            }
            if rec == 0 {
                break;
            }
            at += rec;
        }
        Err(Errno::EIO)
    }
}

/// ext3 のジャーナル(inode の中の領域)。ジャーナルの n 番目のブロックは、inode の n 番目のブロック
struct Journal<'a> {
    fs: &'a Inner,
    inode: Inode,
}

impl<'a> Journal<'a> {
    fn open(fs: &'a Inner, ino: u32) -> Result<Self, Errno> {
        Ok(Self {
            fs,
            inode: fs.inode(ino)?,
        })
    }

    fn place(&self, n: u32) -> Result<u64, Errno> {
        match self.fs.block_of(&self.inode, n as u64)? {
            0 => Err(Errno::EIO),
            b => Ok(b as u64 * self.fs.block_size),
        }
    }
}

impl crate::fs::jbd::JournalIo for Journal<'_> {
    fn block_size(&self) -> usize {
        self.fs.block_size as usize
    }
    fn read_log(&self, n: u32, buf: &mut [u8]) -> Result<(), Errno> {
        self.fs.read_bytes(self.place(n)?, buf)
    }
    fn write_log(&self, n: u32, buf: &[u8]) -> Result<(), Errno> {
        self.fs.write_bytes(self.place(n)?, buf)
    }
    fn write_fs(&self, block: u32, buf: &[u8]) -> Result<(), Errno> {
        self.fs.write_bytes(block as u64 * self.fs.block_size, buf)
    }
}

/// パスを親ディレクトリと名前に分ける(`/a/b` → (`/a`, `b`))
fn split(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(0) => ("/", &path[1..]),
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("/", path),
    }
}

fn kind_of(mode: u16) -> FileKind {
    match mode & 0xf000 {
        0x4000 => FileKind::Directory,
        0x2000 => FileKind::CharDevice,
        0xa000 => FileKind::Symlink,
        _ => FileKind::Regular,
    }
}

/// ext2 の上のファイルやディレクトリ。inode はディスクから毎回読み直す(書き込みで変わるため)
struct Ext2File {
    fs: Arc<Inner>,
    ino: u32,
}

impl Inode {
    /// 短いシンボリックリンク:指す先が inode の中にあり、データブロックを持たない
    fn is_fast_link(&self) -> bool {
        kind_of(self.mode) == FileKind::Symlink && self.sectors == 0
    }
}

impl File for Ext2File {
    fn as_any(&self) -> Option<&dyn core::any::Any> {
        Some(self)
    }

    fn set_mode(&self, mode: u32) -> Result<(), Errno> {
        let _meta = self.fs.meta.lock();
        let mut inode = self.fs.inode(self.ino)?;
        inode.mode = (inode.mode & 0xf000) | (mode as u16 & 0o7777);
        inode.ctime = now_secs();
        self.fs.write_inode(self.ino, &inode)
    }

    fn set_owner(&self, uid: u32, gid: u32) -> Result<(), Errno> {
        let _meta = self.fs.meta.lock();
        let mut inode = self.fs.inode(self.ino)?;
        inode.uid = uid;
        inode.gid = gid;
        inode.ctime = now_secs();
        self.fs.write_inode(self.ino, &inode)
    }

    fn set_times(
        &self,
        atime: Option<crate::fs::vfs::Timespec>,
        mtime: Option<crate::fs::vfs::Timespec>,
    ) -> Result<(), Errno> {
        let _meta = self.fs.meta.lock();
        let mut inode = self.fs.inode(self.ino)?;
        if let Some(t) = atime {
            inode.atime = t.sec as u32;
        }
        if let Some(t) = mtime {
            inode.mtime = t.sec as u32;
        }
        inode.ctime = now_secs();
        self.fs.write_inode(self.ino, &inode)
    }

    fn readlink(&self) -> Result<String, Errno> {
        let inode = self.fs.inode(self.ino)?;
        if kind_of(inode.mode) != FileKind::Symlink {
            return Err(Errno::EINVAL);
        }
        let len = inode.size as usize;
        let bytes = if inode.is_fast_link() {
            let raw: Vec<u8> = inode.blocks.iter().flat_map(|b| b.to_le_bytes()).collect();
            raw[..len.min(FAST_LINK_MAX)].to_vec()
        } else {
            let mut data = vec![0u8; len];
            self.fs.read_file(&inode, 0, &mut data)?;
            data
        };
        String::from_utf8(bytes).map_err(|_| Errno::EIO)
    }

    fn stat(&self) -> Stat {
        let inode = self.fs.inode(self.ino).unwrap_or_default();
        Stat {
            kind: kind_of(inode.mode),
            mode: inode.mode as u32 & 0o7777,
            size: inode.size,
            ino: self.ino as u64,
            uid: inode.uid,
            gid: inode.gid,
            nlink: inode.links as u32,
            atime: secs(inode.atime),
            mtime: secs(inode.mtime),
            ctime: secs(inode.ctime),
        }
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        let inode = self.fs.inode(self.ino)?;
        if kind_of(inode.mode) == FileKind::Directory {
            return Err(Errno::EISDIR);
        }
        self.fs.read_file(&inode, offset, buf)
    }

    fn write(&self, offset: u64, buf: &[u8]) -> Result<usize, Errno> {
        let _meta = self.fs.meta.lock();
        let mut inode = self.fs.inode(self.ino)?;
        if kind_of(inode.mode) == FileKind::Directory {
            return Err(Errno::EISDIR);
        }
        let n = self.fs.write_file(&mut inode, offset, buf)?;
        (inode.mtime, inode.ctime) = (now_secs(), now_secs());
        self.fs.write_inode(self.ino, &inode)?;
        Ok(n)
    }

    fn truncate(&self, len: u64) -> Result<(), Errno> {
        let _meta = self.fs.meta.lock();
        self.fs.truncate(self.ino, len)?;
        let mut inode = self.fs.inode(self.ino)?;
        (inode.mtime, inode.ctime) = (now_secs(), now_secs());
        self.fs.write_inode(self.ino, &inode)
    }
}

pub struct Ext2 {
    inner: Arc<Inner>,
}

impl Ext2 {
    /// ディスクの中の ext2 を読む。ext2 でなければ `EINVAL`。
    pub fn new(disk: SharedDisk) -> Result<Self, Errno> {
        let mut sb = [0u8; 1024];
        let probe = Inner {
            disk: disk.clone(),
            block_size: 1024,
            inodes_per_group: 1,
            inode_size: 128,
            inode_tables: Vec::new(),
            blocks_count: 0,
            blocks_per_group: 0,
            first_data_block: 0,
            gdt_block: 0,
            meta: crate::sync::SpinLock::new(()),
        };
        probe.read_bytes(1024, &mut sb)?;
        if le16(&sb, 56) != MAGIC {
            return Err(Errno::EINVAL);
        }
        let blocks_count = le32(&sb, 4) as u64;
        let first_data_block = le32(&sb, 20) as u64;
        let block_size = 1024u64 << le32(&sb, 24);
        let blocks_per_group = le32(&sb, 32) as u64;
        let inodes_per_group = le32(&sb, 40);
        let inode_size = if le32(&sb, 76) >= 1 {
            le16(&sb, 88) as u64
        } else {
            128
        };
        let groups = (blocks_count - first_data_block).div_ceil(blocks_per_group) as usize;
        // グループ記述子の表は、スーパーブロックの次のブロックから(1 つ 32 バイト)
        let mut gdt = vec![0u8; groups * 32];
        probe.read_bytes((first_data_block + 1) * block_size, &mut gdt)?;
        let inode_tables = (0..groups).map(|g| le32(&gdt, g * 32 + 8)).collect();
        let fs = Self {
            inner: Arc::new(Inner {
                disk,
                block_size,
                inodes_per_group,
                inode_size,
                inode_tables,
                blocks_count,
                blocks_per_group,
                first_data_block,
                gdt_block: first_data_block + 1,
                meta: crate::sync::SpinLock::new(()),
            }),
        };
        // ext3 のジャーナル:前に書きかけで止まっていたら、コミット済みのものを書き直す
        let has_journal = le32(&sb, 92) & COMPAT_HAS_JOURNAL != 0;
        if has_journal && le32(&sb, 96) & INCOMPAT_RECOVER != 0 {
            let journal = Journal::open(&fs.inner, le32(&sb, 224))?;
            crate::fs::jbd::recover(&journal)?;
            let incompat = le32(&sb, 96) & !INCOMPAT_RECOVER;
            fs.inner.write_bytes(1024 + 96, &incompat.to_le_bytes())?;
        }
        Ok(fs)
    }
}

impl Ext2 {
    /// unlink の中身(メタデータのロックは呼ぶ側が持つ)。rename の置き換えでも使う
    fn unlink_locked(&self, path: &str, want_dir: bool) -> Result<(), Errno> {
        let (parent, name) = split(path);

        let (dir_ino, mut dir) = self.inner.lookup(parent)?;
        let (ino, mut inode) = self.inner.lookup(path)?;
        let is_dir = kind_of(inode.mode) == FileKind::Directory;
        match (is_dir, want_dir) {
            (true, false) => return Err(Errno::EISDIR),
            (false, true) => return Err(Errno::ENOTDIR),
            _ => {}
        }
        if is_dir && self.inner.entries(&inode)?.len() > 2 {
            return Err(Errno::ENOTEMPTY);
        }
        self.inner.remove_entry(&dir, name)?;
        if !is_dir && inode.links > 1 {
            // ほかの名前(ハードリンク)が残っている:数を減らすだけ
            inode.links -= 1;
            inode.ctime = now_secs();
            return self.inner.write_inode(ino, &inode);
        }
        if inode.is_fast_link() {
            // ブロック番号の欄は指す先の文字列。ブロックとして返してはいけない
            inode.blocks = [0; 15];
        }
        self.inner.free_from(&mut inode, 0)?;
        inode.links = 0;
        inode.size = 0;
        // 削除時刻(秒)。小さな値だと ext3 の orphan リスト(次の inode 番号)と見なされる
        inode.dtime = (crate::time::BOOT_EPOCH + crate::time::now_ns() / 1_000_000_000) as u32;
        self.inner.write_inode(ino, &inode)?;
        self.inner.free_inode(ino, is_dir)?;
        if is_dir {
            dir = self.inner.inode(dir_ino)?;
            dir.links -= 1;
            self.inner.write_inode(dir_ino, &dir)?;
        }
        Ok(())
    }
}

impl FileSystem for Ext2 {
    /// 名前は VFS を通してしか変わらないので、dentry キャッシュに覚えてよい
    fn cacheable(&self) -> bool {
        true
    }

    /// すでに見つけたディレクトリ `dir` の中だけを探す(ルートからたどり直さない)
    fn lookup_child(
        &self,
        dir: &Arc<dyn File>,
        dir_path: &str,
        name: &str,
    ) -> Result<Arc<dyn File>, Errno> {
        match dir.as_any().and_then(|a| a.downcast_ref::<Ext2File>()) {
            Some(d) if Arc::ptr_eq(&d.fs, &self.inner) => {
                let (ino, _) = self.inner.lookup_in(d.ino, name)?;
                Ok(Arc::new(Ext2File {
                    fs: self.inner.clone(),
                    ino,
                }))
            }
            _ => self.lookup(&crate::fs::vfs::normalize(dir_path, name)),
        }
    }

    fn lookup(&self, path: &str) -> Result<Arc<dyn File>, Errno> {
        let (ino, _) = self.inner.lookup(path)?;
        Ok(Arc::new(Ext2File {
            fs: self.inner.clone(),
            ino,
        }))
    }

    fn create(&self, path: &str, mode: u32) -> Result<Arc<dyn File>, Errno> {
        let _meta = self.inner.meta.lock();
        let (parent, name) = split(path);
        let (dir_ino, dir) = self.inner.lookup(parent)?;
        if kind_of(dir.mode) != FileKind::Directory {
            return Err(Errno::ENOTDIR);
        }
        if self.inner.lookup(path).is_ok() {
            return Err(Errno::EEXIST);
        }
        let ino = self.inner.alloc_inode(false)?;
        let inode = Inode {
            mode: S_IFREG | (mode as u16 & 0o7777),
            links: 1,
            ..Inode::default()
        }
        .born();
        self.inner.write_inode(ino, &inode)?;
        self.inner
            .add_entry(dir_ino, name, ino, FileKind::Regular)?;
        Ok(Arc::new(Ext2File {
            fs: self.inner.clone(),
            ino,
        }))
    }

    fn mkdir(&self, path: &str, mode: u32) -> Result<(), Errno> {
        let _meta = self.inner.meta.lock();
        let (parent, name) = split(path);
        let (dir_ino, mut dir) = self.inner.lookup(parent)?;
        if kind_of(dir.mode) != FileKind::Directory {
            return Err(Errno::ENOTDIR);
        }
        if self.inner.lookup(path).is_ok() {
            return Err(Errno::EEXIST);
        }
        let ino = self.inner.alloc_inode(true)?;
        let mut inode = Inode {
            mode: S_IFDIR | (mode as u16 & 0o7777),
            links: 2, // 親の項目と、自分の「.」
            ..Inode::default()
        }
        .born();
        let block = self.inner.alloc_for(&mut inode)?;
        inode.blocks[0] = block;
        inode.size = self.inner.block_size;
        // 「.」(12 バイト)と「..」(ブロックの残り全部)
        let mut data = vec![0u8; self.inner.block_size as usize];
        data[0..4].copy_from_slice(&ino.to_le_bytes());
        data[4..6].copy_from_slice(&12u16.to_le_bytes());
        data[6] = 1;
        data[7] = 2;
        data[8] = b'.';
        data[12..16].copy_from_slice(&dir_ino.to_le_bytes());
        data[16..18].copy_from_slice(&((self.inner.block_size - 12) as u16).to_le_bytes());
        data[18] = 2;
        data[19] = 2;
        data[20..22].copy_from_slice(b"..");
        self.inner
            .write_bytes(block as u64 * self.inner.block_size, &data)?;
        self.inner.write_inode(ino, &inode)?;
        // 子の「..」が親を指すので、親のリンク数が 1 増える
        dir.links += 1;
        self.inner.write_inode(dir_ino, &dir)?;
        self.inner
            .add_entry(dir_ino, name, ino, FileKind::Directory)
    }

    fn unlink(&self, path: &str, want_dir: bool) -> Result<(), Errno> {
        let _meta = self.inner.meta.lock();
        self.unlink_locked(path, want_dir)
    }

    fn link(&self, old: &str, new: &str) -> Result<(), Errno> {
        let _meta = self.inner.meta.lock();
        let (ino, mut inode) = self.inner.lookup(old)?;
        let kind = kind_of(inode.mode);
        if kind == FileKind::Directory {
            return Err(Errno::EPERM);
        }
        let (parent, name) = split(new);
        let (dir_ino, dir) = self.inner.lookup(parent)?;
        if kind_of(dir.mode) != FileKind::Directory {
            return Err(Errno::ENOTDIR);
        }
        if self.inner.lookup(new).is_ok() {
            return Err(Errno::EEXIST);
        }
        inode.links += 1;
        inode.ctime = now_secs();
        self.inner.write_inode(ino, &inode)?;
        self.inner.add_entry(dir_ino, name, ino, kind)
    }

    fn symlink(&self, path: &str, target: &str) -> Result<(), Errno> {
        let _meta = self.inner.meta.lock();
        let (parent, name) = split(path);
        let (dir_ino, dir) = self.inner.lookup(parent)?;
        if kind_of(dir.mode) != FileKind::Directory {
            return Err(Errno::ENOTDIR);
        }
        if self.inner.lookup(path).is_ok() {
            return Err(Errno::EEXIST);
        }
        let ino = self.inner.alloc_inode(false)?;
        let mut inode = Inode {
            mode: S_IFLNK | 0o777,
            links: 1,
            ..Inode::default()
        }
        .born();
        if target.len() < FAST_LINK_MAX {
            // 短いリンク:ブロック番号の欄に文字列を入れる
            let mut raw = [0u8; FAST_LINK_MAX];
            raw[..target.len()].copy_from_slice(target.as_bytes());
            for (i, b) in inode.blocks.iter_mut().enumerate() {
                *b = u32::from_le_bytes(raw[i * 4..i * 4 + 4].try_into().unwrap());
            }
            inode.size = target.len() as u64;
        } else {
            self.inner.write_file(&mut inode, 0, target.as_bytes())?;
        }
        self.inner.write_inode(ino, &inode)?;
        self.inner.add_entry(dir_ino, name, ino, FileKind::Symlink)
    }

    fn rename(&self, old: &str, new: &str) -> Result<(), Errno> {
        let _meta = self.inner.meta.lock();
        let (old_parent, old_name) = split(old);
        let (new_parent, new_name) = split(new);
        let (old_dir_ino, _) = self.inner.lookup(old_parent)?;
        let (ino, inode) = self.inner.lookup(old)?;
        let (new_dir_ino, new_dir) = self.inner.lookup(new_parent)?;
        if kind_of(new_dir.mode) != FileKind::Directory {
            return Err(Errno::ENOTDIR);
        }
        let kind = kind_of(inode.mode);
        let is_dir = kind == FileKind::Directory;
        // 置き換えられる側を先に消す(ファイルか、空のディレクトリだけ)
        if let Ok((_, target)) = self.inner.lookup(new) {
            let target_kind = kind_of(target.mode);
            let empty =
                target_kind == FileKind::Directory && self.inner.entries(&target)?.len() <= 2;
            crate::fs::vfs::check_replace(kind, target_kind, empty)?;
            self.unlink_locked(new, target_kind == FileKind::Directory)?;
        }
        // 新しい名前を足してから、古い名前を消す(途中で止まっても、ファイルはどこかに残る)
        self.inner.add_entry(new_dir_ino, new_name, ino, kind)?;
        let old_dir = self.inner.inode(old_dir_ino)?;
        self.inner.remove_entry(&old_dir, old_name)?;
        if is_dir && old_dir_ino != new_dir_ino {
            // 移したディレクトリの「..」を新しい親に向け、親のリンク数を付け替える
            self.inner.set_dotdot(&inode, new_dir_ino)?;
            let mut od = self.inner.inode(old_dir_ino)?;
            od.links -= 1;
            self.inner.write_inode(old_dir_ino, &od)?;
            let mut nd = self.inner.inode(new_dir_ino)?;
            nd.links += 1;
            self.inner.write_inode(new_dir_ino, &nd)?;
        }
        Ok(())
    }

    fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, Errno> {
        let (_, inode) = self.inner.lookup(path)?;
        if kind_of(inode.mode) != FileKind::Directory {
            return Err(Errno::ENOTDIR);
        }
        let mut out = Vec::new();
        for (name, ino) in self.inner.entries(&inode)? {
            if name == "." || name == ".." || name == "lost+found" {
                continue;
            }
            let child = self.inner.inode(ino)?;
            out.push(DirEntry {
                name,
                kind: kind_of(child.mode),
                ino: ino as u64,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Errno;
    use crate::block::{ramdisk::RamDisk, shared};
    use crate::fs::vfs::{FileKind, FileSystem};
    use std::boxed::Box;
    use std::string::String;
    use std::vec::Vec;

    /// 一時ディレクトリにファイルを並べ、mke2fs -d で ext2 のイメージを作る
    pub(super) fn image(files: &[(&str, &[u8])]) -> Vec<u8> {
        image_with_links(files, &[])
    }

    /// ファイルと、シンボリックリンク(パス, 指す先)を入れたイメージ
    pub(super) fn image_with_links(files: &[(&str, &[u8])], links: &[(&str, &str)]) -> Vec<u8> {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(std::format!("pios-ext2-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join("root");
        for (path, data) in files {
            let p = root.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, data).unwrap();
        }
        std::fs::create_dir_all(&root).unwrap();
        for (path, target) in links {
            std::os::unix::fs::symlink(target, root.join(path)).unwrap();
        }
        let img = dir.join("fs.img");
        let status = std::process::Command::new("mke2fs")
            .args(["-q", "-F", "-t", "ext2", "-b", "1024", "-I", "128", "-d"])
            .arg(&root)
            .arg(&img)
            .arg("2048")
            .status()
            .expect("mke2fs is needed for the ext2 tests (e2fsprogs)");
        assert!(status.success());
        let bytes = std::fs::read(&img).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        bytes
    }

    fn mount(files: &[(&str, &[u8])]) -> Ext2 {
        Ext2::new(shared(Box::new(RamDisk::from_bytes(&image(files))))).unwrap()
    }

    fn read_all(fs: &Ext2, path: &str) -> Vec<u8> {
        let f = fs.lookup(path).unwrap();
        let mut out = std::vec![0u8; f.stat().size as usize];
        let mut done = 0;
        while done < out.len() {
            let n = f.read(done as u64, &mut out[done..]).unwrap();
            assert!(n > 0);
            done += n;
        }
        out
    }

    #[test]
    fn small_files_are_read_from_their_blocks() {
        let fs = mount(&[("hello.txt", b"Hello from ext2\n")]);
        assert_eq!(read_all(&fs, "/hello.txt"), b"Hello from ext2\n");
        let st = fs.lookup("/hello.txt").unwrap().stat();
        assert_eq!(st.kind, FileKind::Regular);
        assert_eq!(st.size, 16);
    }

    #[test]
    fn large_files_use_indirect_blocks() {
        // 1KiB のブロックで 300KiB:直接 12 + 1 段の間接 256 + 2 段の間接を使う
        let big: Vec<u8> = (0..300 * 1024).map(|i| (i % 251) as u8).collect();
        let fs = mount(&[("big.bin", &big)]);
        assert_eq!(read_all(&fs, "/big.bin"), big);
    }

    #[test]
    fn nested_directories_are_listed_and_followed() {
        let fs = mount(&[("etc/motd", b"hi"), ("etc/conf/a", b"A"), ("top", b"T")]);
        assert_eq!(fs.lookup("/etc").unwrap().stat().kind, FileKind::Directory);
        assert_eq!(read_all(&fs, "/etc/conf/a"), b"A");
        let names: Vec<String> = fs
            .readdir("/etc")
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert!(
            names.contains(&"motd".into()) && names.contains(&"conf".into()),
            "{names:?}"
        );
        assert!(!names.contains(&".".into()), "VFS adds . and .. itself");
    }

    #[test]
    fn symlinks_made_by_mke2fs_are_read() {
        let long = "x/".repeat(50) + "end"; // 103 バイト:ブロックに入る長いリンク
        let fs = Ext2::new(shared(Box::new(RamDisk::from_bytes(&image_with_links(
            &[("hello.txt", b"hi")],
            &[("short", "hello.txt"), ("long", &long)],
        )))))
        .unwrap();
        let short = fs.lookup("/short").unwrap();
        assert_eq!(short.stat().kind, FileKind::Symlink);
        assert_eq!(short.stat().size, 9);
        assert_eq!(short.readlink().unwrap(), "hello.txt");
        assert_eq!(fs.lookup("/long").unwrap().readlink().unwrap(), long);
        let kinds: Vec<(String, FileKind)> = fs
            .readdir("/")
            .unwrap()
            .into_iter()
            .map(|e| (e.name, e.kind))
            .collect();
        assert!(kinds.contains(&(String::from("short"), FileKind::Symlink)));
        assert_eq!(
            fs.lookup("/hello.txt").unwrap().readlink(),
            Err(Errno::EINVAL)
        );
    }

    /// 読んだブロックの数を数えるディスク
    struct Counting {
        disk: RamDisk,
        reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl hal::block::BlockDevice for Counting {
        fn block_count(&self) -> u64 {
            self.disk.block_count()
        }
        fn read_block(
            &mut self,
            lba: u64,
            buf: &mut [u8; BLOCK_SIZE],
        ) -> Result<(), hal::block::BlockError> {
            self.reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.disk.read_block(lba, buf)
        }
        fn write_block(
            &mut self,
            lba: u64,
            buf: &[u8; BLOCK_SIZE],
        ) -> Result<(), hal::block::BlockError> {
            self.disk.write_block(lba, buf)
        }
    }

    #[test]
    fn deep_paths_are_walked_without_rereading_from_the_root() {
        // VFS は 1 要素ずつ(シンボリックリンクを確かめながら)たどる。各要素をルートから
        // 探し直すと、読む量が深さの 2 乗に比例する。前の要素のディレクトリから探せば深さに比例する
        let path = "a/b/c/d/e/f/g/h/file";
        let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let disk = Counting {
            disk: RamDisk::from_bytes(&image(&[(path, b"deep")])),
            reads: reads.clone(),
        };
        let vfs = crate::fs::vfs::Vfs::new(std::sync::Arc::new(
            Ext2::new(shared(Box::new(disk))).unwrap(),
        ));
        reads.store(0, std::sync::atomic::Ordering::Relaxed);
        let f = vfs.lookup(&std::format!("/{path}")).unwrap();
        assert_eq!(f.stat().size, 4);
        let n = reads.load(std::sync::atomic::Ordering::Relaxed);
        let depth = 9;
        assert!(n <= 8 * depth, "{n} sector reads for depth {depth}");
    }

    #[test]
    fn a_path_found_once_is_found_again_without_reading_the_disk() {
        let path = "a/b/c/d/e/f/g/h/file";
        let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let disk = Counting {
            disk: RamDisk::from_bytes(&image(&[(path, b"deep")])),
            reads: reads.clone(),
        };
        let vfs = crate::fs::vfs::Vfs::new(std::sync::Arc::new(
            Ext2::new(shared(Box::new(disk))).unwrap(),
        ));
        let full = std::format!("/{path}");
        vfs.lookup(&full).unwrap();
        reads.store(0, std::sync::atomic::Ordering::Relaxed);
        vfs.lookup(&full).unwrap();
        assert_eq!(reads.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    #[test]
    fn a_missing_name_is_remembered_as_missing() {
        // シェルの PATH 探しや Python の import は、ない名前を何度も探す
        let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let disk = Counting {
            disk: RamDisk::from_bytes(&image(&[("usr/lib/x.py", b"x")])),
            reads: reads.clone(),
        };
        let vfs = crate::fs::vfs::Vfs::new(std::sync::Arc::new(
            Ext2::new(shared(Box::new(disk))).unwrap(),
        ));
        assert_eq!(vfs.lookup("/usr/lib/missing.py").err(), Some(Errno::ENOENT));
        reads.store(0, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(vfs.lookup("/usr/lib/missing.py").err(), Some(Errno::ENOENT));
        assert_eq!(reads.load(std::sync::atomic::Ordering::Relaxed), 0);
        // 作ったら見える(「ない」の記憶は消える)
        vfs.create("/usr/lib/missing.py", 0o644).unwrap();
        assert!(vfs.lookup("/usr/lib/missing.py").is_ok());
    }

    #[test]
    fn missing_paths_and_bad_images() {
        let fs = mount(&[("a", b"x")]);
        assert_eq!(fs.lookup("/nope").err(), Some(Errno::ENOENT));
        assert_eq!(fs.lookup("/a/b").err(), Some(Errno::ENOTDIR));
        let blank = shared(Box::new(RamDisk::new(64)));
        assert!(Ext2::new(blank).is_err(), "no ext2 magic");
    }
}

#[cfg(test)]
mod write_tests {
    use super::*;
    use crate::Errno;
    use crate::block::{SharedDisk, ramdisk::RamDisk, shared};
    use crate::fs::vfs::{FileKind, FileSystem};
    use hal::block::BLOCK_SIZE;
    use std::boxed::Box;
    use std::string::String;
    use std::vec::Vec;

    fn blank() -> SharedDisk {
        shared(Box::new(RamDisk::from_bytes(&super::tests::image(&[(
            "keep.txt", b"keep",
        )]))))
    }

    /// ディスクの中身を取り出して e2fsck -fn に調べさせる(0 ならきれい)
    fn fsck(disk: &SharedDisk) {
        let mut d = disk.lock();
        let mut bytes = Vec::new();
        let mut sector = [0u8; BLOCK_SIZE];
        for lba in 0..d.block_count() {
            d.read_block(lba, &mut sector).unwrap();
            bytes.extend_from_slice(&sector);
        }
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(std::format!("pios-fsck-{}-{n}.img", std::process::id()));
        std::fs::write(&path, &bytes).unwrap();
        let out = std::process::Command::new("e2fsck")
            .args(["-fn"])
            .arg(&path)
            .output()
            .unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(
            out.status.success(),
            "e2fsck found problems:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn read_all(fs: &Ext2, path: &str) -> Vec<u8> {
        let f = fs.lookup(path).unwrap();
        let mut out = std::vec![0u8; f.stat().size as usize];
        let n = f.read(0, &mut out).unwrap();
        assert_eq!(n, out.len());
        out
    }

    #[test]
    fn created_files_survive_a_remount_and_fsck() {
        let disk = blank();
        let data: Vec<u8> = (0..5000).map(|i| (i % 253) as u8).collect();
        {
            let fs = Ext2::new(disk.clone()).unwrap();
            let f = fs.create("/new.txt", 0o644).unwrap();
            assert_eq!(f.write(0, &data), Ok(5000));
            assert_eq!(f.stat().size, 5000);
        }
        let fs = Ext2::new(disk.clone()).unwrap();
        assert_eq!(read_all(&fs, "/new.txt"), data);
        assert_eq!(read_all(&fs, "/keep.txt"), b"keep");
        fsck(&disk);
    }

    #[test]
    fn large_writes_allocate_indirect_blocks() {
        let disk = blank();
        let big: Vec<u8> = (0..300 * 1024).map(|i| (i % 249) as u8).collect();
        {
            let fs = Ext2::new(disk.clone()).unwrap();
            let f = fs.create("/big", 0o600).unwrap();
            for (i, chunk) in big.chunks(7000).enumerate() {
                f.write((i * 7000) as u64, chunk).unwrap();
            }
        }
        assert_eq!(read_all(&Ext2::new(disk.clone()).unwrap(), "/big"), big);
        fsck(&disk);
    }

    #[test]
    fn mkdir_creates_a_directory_that_holds_files() {
        let disk = blank();
        let fs = Ext2::new(disk.clone()).unwrap();
        fs.mkdir("/d", 0o755).unwrap();
        fs.create("/d/a", 0o644).unwrap().write(0, b"A").unwrap();
        assert_eq!(fs.lookup("/d").unwrap().stat().kind, FileKind::Directory);
        let names: Vec<String> = fs
            .readdir("/d")
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["a"]);
        assert_eq!(fs.mkdir("/d", 0o755), Err(Errno::EEXIST));
        fsck(&disk);
    }

    #[test]
    fn many_entries_grow_the_directory() {
        let disk = blank();
        let fs = Ext2::new(disk.clone()).unwrap();
        for i in 0..80 {
            fs.create(&std::format!("/file-with-a-long-name-{i:03}"), 0o644)
                .unwrap();
        }
        assert_eq!(fs.readdir("/").unwrap().len(), 81);
        fsck(&disk);
    }

    #[test]
    fn unlink_and_rmdir_free_everything() {
        let disk = blank();
        let fs = Ext2::new(disk.clone()).unwrap();
        fs.mkdir("/d", 0o755).unwrap();
        fs.create("/d/f", 0o644)
            .unwrap()
            .write(0, &[7u8; 3000])
            .unwrap();
        assert_eq!(fs.unlink("/d", true), Err(Errno::ENOTEMPTY));
        fs.unlink("/d/f", false).unwrap();
        fs.unlink("/d", true).unwrap();
        fs.unlink("/keep.txt", false).unwrap();
        assert_eq!(fs.lookup("/d").err(), Some(Errno::ENOENT));
        assert_eq!(fs.lookup("/keep.txt").err(), Some(Errno::ENOENT));
        fsck(&disk);
    }

    #[test]
    fn truncate_releases_blocks() {
        let disk = blank();
        let fs = Ext2::new(disk.clone()).unwrap();
        let f = fs.create("/t", 0o644).unwrap();
        f.write(0, &[1u8; 20000]).unwrap();
        f.truncate(0).unwrap();
        assert_eq!(f.stat().size, 0);
        f.write(0, b"short").unwrap();
        assert_eq!(read_all(&fs, "/t"), b"short");
        fsck(&disk);
    }

    #[test]
    fn rename_moves_files_between_directories() {
        let disk = blank();
        let fs = Ext2::new(disk.clone()).unwrap();
        fs.mkdir("/d", 0o755).unwrap();
        fs.rename("/keep.txt", "/d/kept.txt").unwrap();
        assert_eq!(fs.lookup("/keep.txt").err(), Some(Errno::ENOENT));
        assert_eq!(read_all(&fs, "/d/kept.txt"), b"keep");
        fs.rename("/d/kept.txt", "/d/again.txt").unwrap();
        assert_eq!(read_all(&fs, "/d/again.txt"), b"keep");
        fsck(&disk);
    }

    #[test]
    fn rename_replaces_a_file_and_frees_the_old_one() {
        let disk = blank();
        let fs = Ext2::new(disk.clone()).unwrap();
        fs.create("/new.tmp", 0o644)
            .unwrap()
            .write(0, b"fresh")
            .unwrap();
        fs.create("/target", 0o644)
            .unwrap()
            .write(0, &[1u8; 5000])
            .unwrap();
        fs.rename("/new.tmp", "/target").unwrap();
        assert_eq!(read_all(&fs, "/target"), b"fresh");
        assert_eq!(fs.lookup("/new.tmp").err(), Some(Errno::ENOENT));
        // 置き換えられた古い /target の inode とブロックが返されていること(e2fsck が確かめる)
        fsck(&disk);
    }

    #[test]
    fn rename_moves_a_directory_and_fixes_its_parent_link() {
        let disk = blank();
        let fs = Ext2::new(disk.clone()).unwrap();
        fs.mkdir("/a", 0o755).unwrap();
        fs.mkdir("/a/sub", 0o755).unwrap();
        fs.create("/a/sub/f", 0o644)
            .unwrap()
            .write(0, b"deep")
            .unwrap();
        fs.mkdir("/b", 0o755).unwrap();
        fs.mkdir("/b/empty", 0o755).unwrap();
        // 別の親へ移す:sub の「..」が /b を指し、/a と /b のリンク数が変わる
        fs.rename("/a/sub", "/b/sub").unwrap();
        assert_eq!(read_all(&fs, "/b/sub/f"), b"deep");
        // 空のディレクトリは置き換えられる
        fs.rename("/b/sub", "/b/empty").unwrap();
        assert_eq!(read_all(&fs, "/b/empty/f"), b"deep");
        assert_eq!(fs.lookup("/b/sub").err(), Some(Errno::ENOENT));
        fsck(&disk);
    }

    #[test]
    fn rename_refuses_bad_replacements() {
        let disk = blank();
        let fs = Ext2::new(disk.clone()).unwrap();
        fs.mkdir("/d", 0o755).unwrap();
        fs.mkdir("/full", 0o755).unwrap();
        fs.create("/full/x", 0o644).unwrap();
        assert_eq!(fs.rename("/keep.txt", "/d"), Err(Errno::EISDIR));
        assert_eq!(fs.rename("/d", "/keep.txt"), Err(Errno::ENOTDIR));
        assert_eq!(fs.rename("/d", "/full"), Err(Errno::ENOTEMPTY));
        assert_eq!(fs.rename("/nope", "/x"), Err(Errno::ENOENT));
        assert_eq!(fs.rename("/keep.txt", "/nodir/x"), Err(Errno::ENOENT));
        fsck(&disk);
    }

    #[test]
    fn symlinks_are_created_and_removed() {
        let disk = blank();
        let long = "/very/long/".repeat(10); // 110 バイト
        {
            let fs = Ext2::new(disk.clone()).unwrap();
            fs.symlink("/short", "keep.txt").unwrap();
            fs.symlink("/long", &long).unwrap();
            fs.mkdir("/d", 0o755).unwrap();
            fs.symlink("/d/up", "..").unwrap();
        }
        let fs = Ext2::new(disk.clone()).unwrap();
        assert_eq!(fs.lookup("/short").unwrap().readlink().unwrap(), "keep.txt");
        assert_eq!(fs.lookup("/long").unwrap().readlink().unwrap(), long);
        assert_eq!(fs.lookup("/short").unwrap().stat().kind, FileKind::Symlink);
        fsck(&disk);
        // 短いリンクの中身(inode の中の文字列)をブロック番号と取り違えて返さないこと
        fs.unlink("/short", false).unwrap();
        fs.unlink("/long", false).unwrap();
        fs.rename("/d/up", "/up").unwrap();
        assert_eq!(fs.lookup("/up").unwrap().readlink().unwrap(), "..");
        fsck(&disk);
    }

    #[test]
    fn mode_and_owner_changes_are_written_to_the_inode() {
        let disk = blank();
        {
            let fs = Ext2::new(disk.clone()).unwrap();
            let f = fs.lookup("/keep.txt").unwrap();
            f.set_mode(0o4750).unwrap();
            // 16 ビットを超える uid(上の 16 ビットは inode の別の場所に入る)
            f.set_owner(70000, 100).unwrap();
        }
        let fs = Ext2::new(disk.clone()).unwrap();
        let st = fs.lookup("/keep.txt").unwrap().stat();
        assert_eq!((st.mode, st.uid, st.gid), (0o4750, 70000, 100));
        assert_eq!(st.kind, FileKind::Regular);
        fsck(&disk);
    }

    #[test]
    fn times_are_kept_in_the_inode() {
        use crate::fs::vfs::Timespec;
        let disk = blank();
        let now = Timespec::now().sec;
        {
            let fs = Ext2::new(disk.clone()).unwrap();
            let f = fs.create("/t", 0o644).unwrap();
            let st = f.stat();
            assert!(st.mtime.sec >= now - 2 && st.ctime.sec >= now - 2, "{st:?}");
            fs.mkdir("/d", 0o755).unwrap();
            assert!(fs.lookup("/d").unwrap().stat().mtime.sec >= now - 2);
            f.set_times(
                Some(Timespec { sec: 1000, nsec: 0 }),
                Some(Timespec { sec: 2000, nsec: 0 }),
            )
            .unwrap();
        }
        let fs = Ext2::new(disk.clone()).unwrap();
        let f = fs.lookup("/t").unwrap();
        assert_eq!((f.stat().atime.sec, f.stat().mtime.sec), (1000, 2000));
        // 書くと mtime が今になる
        f.write(0, b"x").unwrap();
        assert!(f.stat().mtime.sec >= now - 2);
        assert_eq!(f.stat().atime.sec, 1000);
        fsck(&disk);
    }

    #[test]
    fn hard_links_share_the_inode_until_the_last_name_goes() {
        let disk = blank();
        let fs = Ext2::new(disk.clone()).unwrap();
        fs.mkdir("/d", 0o755).unwrap();
        fs.link("/keep.txt", "/d/again").unwrap();
        let a = fs.lookup("/keep.txt").unwrap().stat();
        let b = fs.lookup("/d/again").unwrap().stat();
        assert_eq!((a.ino, a.nlink), (b.ino, 2));
        fsck(&disk);
        // 片方を消しても inode は残る(リンク数 1)
        fs.unlink("/keep.txt", false).unwrap();
        assert_eq!(read_all(&fs, "/d/again"), b"keep");
        assert_eq!(fs.lookup("/d/again").unwrap().stat().nlink, 1);
        fsck(&disk);
        fs.unlink("/d/again", false).unwrap();
        fsck(&disk);
        assert_eq!(fs.link("/d", "/d2"), Err(Errno::EPERM));
    }

    /// ジャーナルのテスト用のトランザクション:(書くブロックと中身の 1 バイト, 取り消すブロック, コミットするか)
    struct Tx {
        writes: &'static [(u32, u8)],
        revoke: &'static [u32],
        commit: bool,
    }

    /// ext3(ジャーナル付き)のイメージを作り、debugfs でジャーナルにトランザクションを書く
    /// (本来の場所には書かない:電源が切れて、リプレイを待っている状態)
    fn ext3_with_journal(txs: &[Tx]) -> SharedDisk {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(std::format!("pios-ext3-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("root")).unwrap();
        std::fs::write(dir.join("root/keep.txt"), b"keep").unwrap();
        let img = dir.join("fs.img");
        let ok = std::process::Command::new("mke2fs")
            .args([
                "-q",
                "-F",
                "-t",
                "ext3",
                "-O",
                "^dir_index",
                "-b",
                "1024",
                "-I",
                "128",
            ])
            .args(["-J", "size=1", "-d"])
            .arg(dir.join("root"))
            .arg(&img)
            .arg("4096")
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let mut cmds = String::from("jo\n");
        for (t, tx) in txs.iter().enumerate() {
            let mut args = String::new();
            let file = dir.join(std::format!("tx{t}"));
            let mut data = Vec::new();
            if !tx.writes.is_empty() {
                let blocks: Vec<String> =
                    tx.writes.iter().map(|(b, _)| std::format!("{b}")).collect();
                args += &std::format!(" -b {}", blocks.join(","));
                for (_, fill) in tx.writes {
                    data.extend_from_slice(&[*fill; 1024]);
                }
            }
            if !tx.revoke.is_empty() {
                let r: Vec<String> = tx.revoke.iter().map(|b| std::format!("{b}")).collect();
                args += &std::format!(" -r {}", r.join(","));
            }
            if !tx.commit {
                args += " -c";
            }
            std::fs::write(&file, &data).unwrap();
            cmds += &std::format!("jw{args} {}\n", file.display());
        }
        cmds += "jc\n";
        std::fs::write(dir.join("cmds"), cmds).unwrap();
        let out = std::process::Command::new("debugfs")
            .args(["-w", "-f"])
            .arg(dir.join("cmds"))
            .arg(&img)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let bytes = std::fs::read(&img).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        shared(Box::new(RamDisk::from_bytes(&bytes)))
    }

    /// ディスクの 1KB のブロック
    fn raw_block(disk: &SharedDisk, block: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let mut sector = [0u8; BLOCK_SIZE];
        for i in 0..2 {
            disk.lock()
                .read_block(block as u64 * 2 + i, &mut sector)
                .unwrap();
            out.extend_from_slice(&sector);
        }
        out
    }

    #[test]
    fn committed_transactions_are_replayed_on_mount() {
        let disk = ext3_with_journal(&[Tx {
            writes: &[(3000, b'J'), (3001, b'K')],
            revoke: &[],
            commit: true,
        }]);
        assert_eq!(raw_block(&disk, 3000), [0; 1024], "not yet in place");
        let fs = Ext2::new(disk.clone()).unwrap();
        assert_eq!(raw_block(&disk, 3000), [b'J'; 1024]);
        assert_eq!(raw_block(&disk, 3001), [b'K'; 1024]);
        assert_eq!(read_all(&fs, "/keep.txt"), b"keep");
        // リプレイしたら「要リカバリ」の印を消し、ジャーナルを空にする(e2fsck が確かめる)
        fsck(&disk);
    }

    #[test]
    fn an_uncommitted_transaction_is_ignored() {
        let disk = ext3_with_journal(&[Tx {
            writes: &[(3000, b'X')],
            revoke: &[],
            commit: false,
        }]);
        Ext2::new(disk.clone()).unwrap();
        assert_eq!(raw_block(&disk, 3000), [0; 1024]);
        fsck(&disk);
    }

    #[test]
    fn later_transactions_win_and_revoked_blocks_are_skipped() {
        let disk = ext3_with_journal(&[
            Tx {
                writes: &[(3000, b'A'), (3002, b'C')],
                revoke: &[],
                commit: true,
            },
            Tx {
                writes: &[(3000, b'B')],
                revoke: &[],
                commit: true,
            },
            // 3002 は取り消された(後で解放されて別の用途になった、など):古い中身を書いてはいけない
            Tx {
                writes: &[(3003, b'D')],
                revoke: &[3002],
                commit: true,
            },
        ]);
        Ext2::new(disk.clone()).unwrap();
        assert_eq!(raw_block(&disk, 3000), [b'B'; 1024]);
        assert_eq!(raw_block(&disk, 3002), [0; 1024]);
        assert_eq!(raw_block(&disk, 3003), [b'D'; 1024]);
        fsck(&disk);
    }
}
