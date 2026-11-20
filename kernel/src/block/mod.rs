//! ブロック層(ディスクとパーティション)。
//!
//! ドライバが作ったブロックデバイスを名前(`vda` など)で登録しておき、devfs が `/dev/<名前>` として見せる。

pub mod mbr;
pub mod ramdisk;

use crate::Errno;
use crate::fs::vfs::{File, FileKind, Stat};
use crate::sync::SpinLock;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use hal::block::{BLOCK_SIZE, BlockDevice};

/// 共有できるブロックデバイス
pub type SharedDisk = Arc<SpinLock<Box<dyn BlockDevice>>>;

pub fn shared(dev: Box<dyn BlockDevice>) -> SharedDisk {
    Arc::new(SpinLock::new(dev))
}

static DISKS: SpinLock<Vec<(String, SharedDisk)>> = SpinLock::new(Vec::new());

/// ブロックデバイスを名前で登録する
pub fn register(name: &str, dev: SharedDisk) {
    DISKS.lock().push((String::from(name), dev));
}

pub fn find(name: &str) -> Option<SharedDisk> {
    DISKS
        .lock()
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, d)| d.clone())
}

pub fn names() -> Vec<String> {
    DISKS.lock().iter().map(|(n, _)| n.clone()).collect()
}

/// ブロックデバイスを、バイト単位の位置で読み書きできるファイルとして見せる(`/dev/vda`)。
pub struct BlockFile {
    disk: SharedDisk,
}

impl BlockFile {
    pub fn new(disk: SharedDisk) -> Self {
        Self { disk }
    }

    fn size(&self) -> u64 {
        self.disk.lock().block_count() * BLOCK_SIZE as u64
    }
}

fn io(_: hal::block::BlockError) -> Errno {
    Errno::EIO
}

impl File for BlockFile {
    fn stat(&self) -> Stat {
        Stat {
            kind: FileKind::Regular,
            mode: 0o660,
            size: self.size(),
            ino: 0x5_0000,
            uid: 0,
            gid: 0,
        }
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        let size = self.size();
        let len = (buf.len() as u64).min(size.saturating_sub(offset)) as usize;
        let mut disk = self.disk.lock();
        let mut sector = [0u8; BLOCK_SIZE];
        let mut done = 0;
        while done < len {
            let pos = offset + done as u64;
            let lba = pos / BLOCK_SIZE as u64;
            let within = (pos % BLOCK_SIZE as u64) as usize;
            disk.read_block(lba, &mut sector).map_err(io)?;
            let n = (BLOCK_SIZE - within).min(len - done);
            buf[done..done + n].copy_from_slice(&sector[within..within + n]);
            done += n;
        }
        Ok(len)
    }

    fn write(&self, offset: u64, buf: &[u8]) -> Result<usize, Errno> {
        let size = self.size();
        let len = (buf.len() as u64).min(size.saturating_sub(offset)) as usize;
        let mut disk = self.disk.lock();
        let mut sector = [0u8; BLOCK_SIZE];
        let mut done = 0;
        while done < len {
            let pos = offset + done as u64;
            let lba = pos / BLOCK_SIZE as u64;
            let within = (pos % BLOCK_SIZE as u64) as usize;
            let n = (BLOCK_SIZE - within).min(len - done);
            // セクタの一部だけを書くときは、読んでから書き換える
            if n < BLOCK_SIZE {
                disk.read_block(lba, &mut sector).map_err(io)?;
            }
            sector[within..within + n].copy_from_slice(&buf[done..done + n]);
            disk.write_block(lba, &sector).map_err(io)?;
            done += n;
        }
        Ok(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::vfs::File;
    use alloc::boxed::Box;
    use hal::block::BlockDevice;

    fn disk() -> BlockFile {
        let mut d = ramdisk::RamDisk::new(4);
        let mut sector = [0u8; 512];
        sector[510..].copy_from_slice(b"AB");
        d.write_block(0, &sector).unwrap();
        sector = [0u8; 512];
        sector[..2].copy_from_slice(b"CD");
        d.write_block(1, &sector).unwrap();
        BlockFile::new(shared(Box::new(d)))
    }

    #[test]
    fn reads_cross_sector_boundaries() {
        let f = disk();
        let mut buf = [0u8; 4];
        assert_eq!(f.read(510, &mut buf), Ok(4));
        assert_eq!(&buf, b"ABCD");
        assert_eq!(f.stat().size, 4 * 512);
    }

    #[test]
    fn reads_stop_at_the_end_of_the_disk() {
        let f = disk();
        let mut buf = [0u8; 16];
        assert_eq!(f.read(4 * 512 - 4, &mut buf), Ok(4));
        assert_eq!(f.read(4 * 512, &mut buf), Ok(0));
    }

    #[test]
    fn writes_modify_only_the_given_bytes() {
        let f = disk();
        assert_eq!(f.write(511, b"xy"), Ok(2));
        let mut buf = [0u8; 4];
        f.read(510, &mut buf).unwrap();
        assert_eq!(&buf, b"AxyD");
    }

    #[test]
    fn registered_devices_are_found_by_name() {
        register("testdisk", shared(Box::new(ramdisk::RamDisk::new(1))));
        assert!(find("testdisk").is_some());
        assert!(names().iter().any(|n| n == "testdisk"));
        assert!(find("nope").is_none());
    }
}
