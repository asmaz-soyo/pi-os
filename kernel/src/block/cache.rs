//! ブロックキャッシュ:最近読んだセクタをメモリに残し、同じセクタはディスクから読み直さない。
//!
//! 書き込みはディスクにもそのまま書く(write-through)。電源が切れても、キャッシュにだけあって
//! ディスクにないデータはない(その代わり、書き込みは速くならない)。
//! いっぱいになったら、一番長く使われていないセクタを捨てる(LRU)。

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use hal::block::{BLOCK_SIZE, BlockDevice, BlockError};

struct Entry {
    data: Box<[u8; BLOCK_SIZE]>,
    /// 最後に使った順番(大きいほど最近)
    used: u64,
}

pub struct CachedDevice {
    inner: Box<dyn BlockDevice>,
    capacity: usize,
    entries: BTreeMap<u64, Entry>,
    /// 使った順番 → セクタ。先頭が一番長く使われていないもの(捨てるものを全部見ずに選べる)
    order: BTreeMap<u64, u64>,
    clock: u64,
}

impl CachedDevice {
    /// `capacity` 個のセクタまで覚えておく
    pub fn new(inner: Box<dyn BlockDevice>, capacity: usize) -> Self {
        Self {
            inner,
            capacity: capacity.max(1),
            entries: BTreeMap::new(),
            order: BTreeMap::new(),
            clock: 0,
        }
    }

    /// `lba` を今使ったことにする
    fn touch(&mut self, lba: u64) {
        self.clock += 1;
        let now = self.clock;
        if let Some(e) = self.entries.get_mut(&lba) {
            self.order.remove(&e.used);
            e.used = now;
            self.order.insert(now, lba);
        }
    }

    /// セクタを覚える。いっぱいなら、一番長く使われていないものを捨てる
    fn remember(&mut self, lba: u64, data: &[u8; BLOCK_SIZE]) {
        if let Some(e) = self.entries.get_mut(&lba) {
            e.data.copy_from_slice(data);
            self.touch(lba);
            return;
        }
        // 捨てるもののバッファを使い回す(セクタごとに確保し直さない)
        let buffer = if self.entries.len() >= self.capacity {
            let (_, oldest) = self.order.pop_first().expect("cache is full");
            self.entries.remove(&oldest).map(|e| e.data)
        } else {
            None
        };
        let mut data_box = buffer.unwrap_or_else(|| Box::new([0; BLOCK_SIZE]));
        data_box.copy_from_slice(data);
        self.entries.insert(
            lba,
            Entry {
                data: data_box,
                used: 0,
            },
        );
        // 順番は 1 から数えるので、used: 0 は順番の表にない。touch で表に入る
        self.touch(lba);
    }
}

impl BlockDevice for CachedDevice {
    fn block_count(&self) -> u64 {
        self.inner.block_count()
    }

    fn read_block(&mut self, lba: u64, buf: &mut [u8; BLOCK_SIZE]) -> Result<(), BlockError> {
        if let Some(e) = self.entries.get(&lba) {
            buf.copy_from_slice(&*e.data);
            self.touch(lba);
            return Ok(());
        }
        self.inner.read_block(lba, buf)?;
        self.remember(lba, buf);
        Ok(())
    }

    fn write_block(&mut self, lba: u64, buf: &[u8; BLOCK_SIZE]) -> Result<(), BlockError> {
        self.inner.write_block(lba, buf)?;
        self.remember(lba, buf);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::ramdisk::RamDisk;
    use std::boxed::Box;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 読んだ回数を数えるディスク
    struct Counting(RamDisk, Arc<AtomicUsize>);

    impl BlockDevice for Counting {
        fn block_count(&self) -> u64 {
            self.0.block_count()
        }
        fn read_block(&mut self, lba: u64, buf: &mut [u8; BLOCK_SIZE]) -> Result<(), BlockError> {
            self.1.fetch_add(1, Ordering::Relaxed);
            self.0.read_block(lba, buf)
        }
        fn write_block(&mut self, lba: u64, buf: &[u8; BLOCK_SIZE]) -> Result<(), BlockError> {
            self.0.write_block(lba, buf)
        }
    }

    fn cached(capacity: usize) -> (CachedDevice, Arc<AtomicUsize>) {
        let reads = Arc::new(AtomicUsize::new(0));
        let mut disk = RamDisk::new(64);
        for lba in 0..64u64 {
            disk.write_block(lba, &[lba as u8; BLOCK_SIZE]).unwrap();
        }
        (
            CachedDevice::new(Box::new(Counting(disk, reads.clone())), capacity),
            reads,
        )
    }

    #[test]
    fn a_sector_read_twice_comes_from_memory_the_second_time() {
        let (mut dev, reads) = cached(8);
        let mut buf = [0u8; BLOCK_SIZE];
        dev.read_block(5, &mut buf).unwrap();
        dev.read_block(5, &mut buf).unwrap();
        assert_eq!(buf, [5; BLOCK_SIZE]);
        assert_eq!(reads.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn writes_go_to_the_disk_and_the_cache() {
        let (mut dev, reads) = cached(8);
        let mut buf = [0u8; BLOCK_SIZE];
        dev.read_block(3, &mut buf).unwrap();
        dev.write_block(3, &[0xaa; BLOCK_SIZE]).unwrap();
        dev.read_block(3, &mut buf).unwrap();
        assert_eq!(buf, [0xaa; BLOCK_SIZE], "the cache holds the new data");
        assert_eq!(reads.load(Ordering::Relaxed), 1);
        // ディスクにも書かれている(書き込みはそのまま通す:write-through)
        dev.inner.read_block(3, &mut buf).unwrap();
        assert_eq!(buf, [0xaa; BLOCK_SIZE]);
    }

    #[test]
    fn the_least_recently_used_sector_is_dropped_when_full() {
        let (mut dev, reads) = cached(2);
        let mut buf = [0u8; BLOCK_SIZE];
        dev.read_block(1, &mut buf).unwrap();
        dev.read_block(2, &mut buf).unwrap();
        dev.read_block(1, &mut buf).unwrap(); // 1 を最近使った
        dev.read_block(3, &mut buf).unwrap(); // いっぱい:一番使っていない 2 を捨てる
        assert_eq!(reads.load(Ordering::Relaxed), 3);
        dev.read_block(1, &mut buf).unwrap();
        assert_eq!(reads.load(Ordering::Relaxed), 3, "1 is still cached");
        dev.read_block(2, &mut buf).unwrap();
        assert_eq!(reads.load(Ordering::Relaxed), 4, "2 was dropped");
        assert_eq!(buf, [2; BLOCK_SIZE]);
    }
}
