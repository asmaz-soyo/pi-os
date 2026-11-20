//! ext2 ファイルシステム(今は読み出し専用)。
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
const ROOT_INO: u32 = 2;

struct Inner {
    disk: SharedDisk,
    block_size: u64,
    inodes_per_group: u32,
    inode_size: u64,
    /// グループごとの inode 表の最初のブロック
    inode_tables: Vec<u32>,
}

#[derive(Debug, Clone, Copy)]
struct Inode {
    mode: u16,
    uid: u32,
    gid: u32,
    size: u64,
    blocks: [u32; 15],
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
            uid: le16(&raw, 2) as u32,
            gid: le16(&raw, 24) as u32,
            size: le32(&raw, 4) as u64 | ((le32(&raw, 108) as u64) << 32),
            blocks,
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

fn kind_of(mode: u16) -> FileKind {
    match mode & 0xf000 {
        0x4000 => FileKind::Directory,
        0x2000 => FileKind::CharDevice,
        _ => FileKind::Regular,
    }
}

/// ext2 の上のファイルやディレクトリ
struct Ext2File {
    fs: Arc<Inner>,
    ino: u32,
    inode: Inode,
}

impl File for Ext2File {
    fn stat(&self) -> Stat {
        Stat {
            kind: kind_of(self.inode.mode),
            mode: self.inode.mode as u32 & 0o7777,
            size: self.inode.size,
            ino: self.ino as u64,
            uid: self.inode.uid,
            gid: self.inode.gid,
        }
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        if kind_of(self.inode.mode) == FileKind::Directory {
            return Err(Errno::EISDIR);
        }
        self.fs.read_file(&self.inode, offset, buf)
    }

    fn write(&self, _offset: u64, _buf: &[u8]) -> Result<usize, Errno> {
        Err(Errno::EROFS)
    }

    fn is_read_only(&self) -> bool {
        true
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
        Ok(Self {
            inner: Arc::new(Inner {
                disk,
                block_size,
                inodes_per_group,
                inode_size,
                inode_tables,
            }),
        })
    }
}

impl FileSystem for Ext2 {
    fn lookup(&self, path: &str) -> Result<Arc<dyn File>, Errno> {
        let (ino, inode) = self.inner.lookup(path)?;
        Ok(Arc::new(Ext2File {
            fs: self.inner.clone(),
            ino,
            inode,
        }))
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
    fn image(files: &[(&str, &[u8])]) -> Vec<u8> {
        let dir = std::env::temp_dir().join(std::format!(
            "pios-ext2-{}-{}",
            std::process::id(),
            files.len() * 1000 + files.iter().map(|f| f.1.len()).sum::<usize>() % 997
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join("root");
        for (path, data) in files {
            let p = root.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, data).unwrap();
        }
        std::fs::create_dir_all(&root).unwrap();
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
    fn missing_paths_and_bad_images() {
        let fs = mount(&[("a", b"x")]);
        assert_eq!(fs.lookup("/nope").err(), Some(Errno::ENOENT));
        assert_eq!(fs.lookup("/a/b").err(), Some(Errno::ENOTDIR));
        let blank = shared(Box::new(RamDisk::new(64)));
        assert!(Ext2::new(blank).is_err(), "no ext2 magic");
    }
}
