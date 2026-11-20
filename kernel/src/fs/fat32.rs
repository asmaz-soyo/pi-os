//! FAT32 ファイルシステム(読み取り専用)。SD カードの最初のパーティション(`/boot`)用。
//!
//! ```text
//! セクタ 0          ブートセクタ(BPB:セクタの大きさ、クラスタの大きさ、FAT の場所と大きさ、ルートのクラスタ)
//! 予約領域の後      FAT(クラスタごとに「次のクラスタ」の番号。0x0FFFFFF8 以上で終わり)× 2 つ
//! その後            データ領域(クラスタ 2 から)
//! ```
//!
//! ファイルもディレクトリも「クラスタの鎖」。ディレクトリの中身は 32 バイトのエントリの並び。
//! 8.3 に収まらない名前は、本体のエントリの前に長い名前(LFN、UTF-16 で 13 文字ずつ)のエントリが付く。
//! パーミッションや所有者はないので、すべて root の持ち物で、ファイルは 0o644、ディレクトリは 0o755 とする。

use crate::Errno;
use crate::block::SharedDisk;
use crate::fs::vfs::{DirEntry, File, FileKind, FileSystem, Stat};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use hal::block::BLOCK_SIZE;

const ATTR_VOLUME: u8 = 0x08;
const ATTR_DIR: u8 = 0x10;
const ATTR_LFN: u8 = 0x0F;
/// これ以上は鎖の終わり
const END_OF_CHAIN: u32 = 0x0FFF_FFF8;

struct Inner {
    disk: SharedDisk,
    /// クラスタの大きさ(バイト)
    cluster_size: u64,
    /// FAT の始まり(バイト)
    fat_start: u64,
    /// データ領域の始まり(バイト)
    data_start: u64,
    root_cluster: u32,
    clusters: u32,
}

/// ディレクトリの中の 1 項目
#[derive(Debug, Clone)]
struct Entry {
    name: String,
    dir: bool,
    cluster: u32,
    size: u32,
}

fn le16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn le32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

impl Inner {
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

    /// FAT を引いて、`cluster` の次のクラスタ(終わりなら None)
    fn next_cluster(&self, cluster: u32) -> Result<Option<u32>, Errno> {
        let mut b = [0u8; 4];
        self.read_bytes(self.fat_start + cluster as u64 * 4, &mut b)?;
        let next = u32::from_le_bytes(b) & 0x0FFF_FFFF;
        match next {
            n if n >= END_OF_CHAIN => Ok(None),
            n if n < 2 || n >= self.clusters + 2 => Err(Errno::EIO),
            n => Ok(Some(n)),
        }
    }

    fn cluster_offset(&self, cluster: u32) -> u64 {
        self.data_start + (cluster as u64 - 2) * self.cluster_size
    }

    /// 鎖の中の `index` 番目のクラスタ
    fn nth_cluster(&self, first: u32, index: u64) -> Result<u32, Errno> {
        let mut c = first;
        for _ in 0..index {
            c = self.next_cluster(c)?.ok_or(Errno::EIO)?;
        }
        Ok(c)
    }

    /// 鎖をたどって、ディレクトリの中身(32 バイトのエントリの並び)をすべて読む
    fn read_chain(&self, first: u32) -> Result<Vec<u8>, Errno> {
        let mut out = Vec::new();
        let mut c = Some(first);
        while let Some(cluster) = c {
            let start = out.len();
            out.resize(start + self.cluster_size as usize, 0);
            self.read_bytes(self.cluster_offset(cluster), &mut out[start..])?;
            c = self.next_cluster(cluster)?;
            if out.len() > 64 << 20 {
                return Err(Errno::EIO); // 鎖が輪になっている
            }
        }
        Ok(out)
    }

    /// ディレクトリの項目を並べる(`.` と `..`、ボリュームラベル、消したものは除く)
    fn entries(&self, dir_cluster: u32) -> Result<Vec<Entry>, Errno> {
        let raw = self.read_chain(dir_cluster)?;
        let mut out = Vec::new();
        // 長い名前の部品(後ろの部分が先に来るので、番号の位置に置く)
        let mut lfn: Vec<u16> = Vec::new();
        for e in raw.as_chunks::<32>().0 {
            match e[0] {
                0x00 => break, // ここから先は空
                0xE5 => {
                    lfn.clear(); // 消したエントリ
                    continue;
                }
                _ => {}
            }
            let attr = e[11];
            if attr == ATTR_LFN {
                let seq = (e[0] & 0x1F) as usize;
                if seq == 0 {
                    continue;
                }
                if e[0] & 0x40 != 0 {
                    lfn = vec![0xFFFF; seq * 13];
                }
                if lfn.len() < seq * 13 {
                    continue;
                }
                let slots = (1..11)
                    .step_by(2)
                    .chain((14..26).step_by(2))
                    .chain((28..32).step_by(2));
                for (i, slot) in slots.enumerate() {
                    lfn[(seq - 1) * 13 + i] = le16(e, slot);
                }
                continue;
            }
            if attr & ATTR_VOLUME != 0 {
                lfn.clear();
                continue;
            }
            let short = short_name(e);
            if short == "." || short == ".." {
                lfn.clear();
                continue;
            }
            let long: Vec<u16> = lfn
                .iter()
                .copied()
                .take_while(|&u| u != 0 && u != 0xFFFF)
                .collect();
            let name = if long.is_empty() {
                short
            } else {
                char::decode_utf16(long)
                    .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
                    .collect()
            };
            lfn.clear();
            out.push(Entry {
                name,
                dir: attr & ATTR_DIR != 0,
                cluster: (le16(e, 20) as u32) << 16 | le16(e, 26) as u32,
                size: le32(e, 28),
            });
        }
        Ok(out)
    }

    /// パスをたどって項目を探す。ルートは大きさ 0 のディレクトリとして返す
    fn lookup(&self, path: &str) -> Result<Entry, Errno> {
        let mut cur = Entry {
            name: String::new(),
            dir: true,
            cluster: self.root_cluster,
            size: 0,
        };
        for part in path.split('/').filter(|p| !p.is_empty()) {
            if !cur.dir {
                return Err(Errno::ENOTDIR);
            }
            cur = self
                .entries(cur.cluster)?
                .into_iter()
                .find(|e| e.name.eq_ignore_ascii_case(part))
                .ok_or(Errno::ENOENT)?;
            // ルートを指す「..」などはクラスタ 0 で書かれている
            if cur.dir && cur.cluster == 0 {
                cur.cluster = self.root_cluster;
            }
        }
        Ok(cur)
    }

    fn read_file(&self, e: &Entry, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        let size = e.size as u64;
        if offset >= size {
            return Ok(0);
        }
        let len = buf.len().min((size - offset) as usize);
        let mut done = 0;
        // 最初のクラスタまで鎖をたどり、あとは 1 つずつ進む
        let mut cluster = self.nth_cluster(e.cluster, offset / self.cluster_size)?;
        while done < len {
            let pos = offset + done as u64;
            let within = pos % self.cluster_size;
            let n = ((self.cluster_size - within) as usize).min(len - done);
            self.read_bytes(
                self.cluster_offset(cluster) + within,
                &mut buf[done..done + n],
            )?;
            done += n;
            if done < len {
                cluster = self.next_cluster(cluster)?.ok_or(Errno::EIO)?;
            }
        }
        Ok(len)
    }
}

/// 8.3 の名前を「BASE.EXT」にする。Windows NT の小文字の印(12 バイト目)も見る
fn short_name(e: &[u8]) -> String {
    let lower_base = e[12] & 0x08 != 0;
    let lower_ext = e[12] & 0x10 != 0;
    let part = |b: &[u8], lower: bool| -> String {
        let s = core::str::from_utf8(b).unwrap_or("?").trim_end();
        if lower {
            s.to_ascii_lowercase()
        } else {
            String::from(s)
        }
    };
    let mut first = e[..8].to_vec();
    if first[0] == 0x05 {
        first[0] = 0xE5; // 先頭が 0xE5 の名前は 0x05 で書かれる
    }
    let base = part(&first, lower_base);
    let ext = part(&e[8..11], lower_ext);
    if ext.is_empty() {
        base
    } else {
        alloc::format!("{base}.{ext}")
    }
}

/// FAT32 の上のファイルやディレクトリ
struct FatFile {
    fs: Arc<Inner>,
    entry: Entry,
}

impl File for FatFile {
    fn stat(&self) -> Stat {
        Stat {
            kind: if self.entry.dir {
                FileKind::Directory
            } else {
                FileKind::Regular
            },
            mode: if self.entry.dir { 0o755 } else { 0o644 },
            size: self.entry.size as u64,
            // inode 番号の代わりに、最初のクラスタを使う
            ino: self.entry.cluster as u64,
            uid: 0,
            gid: 0,
        }
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        if self.entry.dir {
            return Err(Errno::EISDIR);
        }
        self.fs.read_file(&self.entry, offset, buf)
    }

    fn write(&self, _offset: u64, _buf: &[u8]) -> Result<usize, Errno> {
        Err(Errno::EROFS)
    }

    fn truncate(&self, _len: u64) -> Result<(), Errno> {
        Err(Errno::EROFS)
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

pub struct Fat32 {
    inner: Arc<Inner>,
}

impl Fat32 {
    /// ディスクの中の FAT32 を読む。FAT32 でなければ `EINVAL`。
    pub fn new(disk: SharedDisk) -> Result<Self, Errno> {
        let mut bs = [0u8; BLOCK_SIZE];
        disk.lock().read_block(0, &mut bs).map_err(|_| Errno::EIO)?;
        let bytes_per_sector = le16(&bs, 11) as u64;
        let sectors_per_cluster = bs[13] as u64;
        let reserved = le16(&bs, 14) as u64;
        let fats = bs[16] as u64;
        let fat_size16 = le16(&bs, 22);
        let total = le32(&bs, 32) as u64;
        let fat_size = le32(&bs, 36) as u64;
        // 署名 0x55AA、FAT16 用の FAT の大きさが 0、ファイルシステムの種類が "FAT32"
        let is_fat32 = bs[510] == 0x55
            && bs[511] == 0xAA
            && fat_size16 == 0
            && &bs[82..87] == b"FAT32"
            && bytes_per_sector == BLOCK_SIZE as u64
            && sectors_per_cluster.is_power_of_two()
            && fats > 0;
        if !is_fat32 {
            return Err(Errno::EINVAL);
        }
        let data_sector = reserved + fats * fat_size;
        Ok(Self {
            inner: Arc::new(Inner {
                disk,
                cluster_size: bytes_per_sector * sectors_per_cluster,
                fat_start: reserved * bytes_per_sector,
                data_start: data_sector * bytes_per_sector,
                root_cluster: le32(&bs, 44),
                clusters: ((total - data_sector) / sectors_per_cluster) as u32,
            }),
        })
    }
}

impl FileSystem for Fat32 {
    fn lookup(&self, path: &str) -> Result<Arc<dyn File>, Errno> {
        let entry = self.inner.lookup(path)?;
        Ok(Arc::new(FatFile {
            fs: self.inner.clone(),
            entry,
        }))
    }

    fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, Errno> {
        let dir = self.inner.lookup(path)?;
        if !dir.dir {
            return Err(Errno::ENOTDIR);
        }
        Ok(self
            .inner
            .entries(dir.cluster)?
            .into_iter()
            .map(|e| DirEntry {
                kind: if e.dir {
                    FileKind::Directory
                } else {
                    FileKind::Regular
                },
                ino: e.cluster as u64,
                name: e.name,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{ramdisk::RamDisk, shared};
    use crate::fs::vfs::{FileKind, FileSystem};
    use std::boxed::Box;
    use std::string::String;
    use std::vec::Vec;

    fn mount(files: &[(&str, &[u8])]) -> Fat32 {
        let img = fatimg::build(34, "PIOSBOOT", files);
        Fat32::new(shared(Box::new(RamDisk::from_bytes(&img)))).unwrap()
    }

    fn read_all(fs: &Fat32, path: &str) -> Vec<u8> {
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
    fn files_with_short_names_are_read() {
        let fs = mount(&[("CONFIG.TXT", b"arm_64bit=1\n")]);
        assert_eq!(read_all(&fs, "/CONFIG.TXT"), b"arm_64bit=1\n");
        let st = fs.lookup("/CONFIG.TXT").unwrap().stat();
        assert_eq!(st.kind, FileKind::Regular);
        assert_eq!(st.size, 12);
    }

    #[test]
    fn long_names_and_subdirectories_are_found() {
        let fs = mount(&[
            ("kernel8.img", b"KERNEL"),
            ("overlays/disable-bt.dtbo", b"dtbo"),
        ]);
        assert_eq!(read_all(&fs, "/kernel8.img"), b"KERNEL");
        assert_eq!(read_all(&fs, "/overlays/disable-bt.dtbo"), b"dtbo");
        assert_eq!(
            fs.lookup("/overlays").unwrap().stat().kind,
            FileKind::Directory
        );
        assert_eq!(fs.lookup("/nothing").err(), Some(Errno::ENOENT));
        assert_eq!(fs.lookup("/kernel8.img/x").err(), Some(Errno::ENOTDIR));
    }

    #[test]
    fn names_match_without_case() {
        // FAT は大文字と小文字を区別しない
        let fs = mount(&[("CONFIG.TXT", b"x"), ("kernel8.img", b"y")]);
        assert_eq!(read_all(&fs, "/config.txt"), b"x");
        assert_eq!(read_all(&fs, "/KERNEL8.IMG"), b"y");
    }

    #[test]
    fn files_spanning_many_clusters_follow_the_chain() {
        let big: Vec<u8> = (0..5000u32).map(|i| (i * 7 % 251) as u8).collect();
        let fs = mount(&[("big.bin", &big)]);
        assert_eq!(read_all(&fs, "/big.bin"), big);
        // 途中から読む
        let f = fs.lookup("/big.bin").unwrap();
        let mut buf = [0u8; 10];
        assert_eq!(f.read(4995, &mut buf).unwrap(), 5);
        assert_eq!(&buf[..5], &big[4995..]);
        assert_eq!(f.read(5000, &mut buf).unwrap(), 0);
    }

    #[test]
    fn readdir_lists_long_names() {
        let fs = mount(&[
            ("CONFIG.TXT", b"x"),
            ("kernel8.img", b"y"),
            ("overlays/a.dtbo", b"z"),
        ]);
        let mut names: Vec<(String, FileKind)> = fs
            .readdir("/")
            .unwrap()
            .into_iter()
            .map(|e| (e.name, e.kind))
            .collect();
        names.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            names,
            [
                (String::from("CONFIG.TXT"), FileKind::Regular),
                (String::from("kernel8.img"), FileKind::Regular),
                (String::from("overlays"), FileKind::Directory),
            ]
        );
        let sub: Vec<String> = fs
            .readdir("/overlays")
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(sub, ["a.dtbo"]);
    }

    #[test]
    fn it_is_read_only() {
        let fs = mount(&[("CONFIG.TXT", b"x")]);
        let f = fs.lookup("/CONFIG.TXT").unwrap();
        assert!(f.is_read_only());
        assert_eq!(f.write(0, b"y"), Err(Errno::EROFS));
        assert_eq!(fs.create("/new", 0o644).err(), Some(Errno::EROFS));
        assert_eq!(fs.mkdir("/d", 0o755), Err(Errno::EROFS));
        assert_eq!(fs.unlink("/CONFIG.TXT", false), Err(Errno::EROFS));
    }

    #[test]
    fn other_filesystems_are_rejected() {
        let zeros = std::vec![0u8; 1 << 20];
        assert_eq!(
            Fat32::new(shared(Box::new(RamDisk::from_bytes(&zeros)))).err(),
            Some(Errno::EINVAL)
        );
    }
}
