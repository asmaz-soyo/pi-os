//! ブロックキャッシュ:最近読んだセクタをメモリに残し、同じセクタはディスクから読み直さない。
//!
//! 書き込みは 2 通り:
//! - write-through(`new`):ディスクにもすぐ書く。キャッシュにだけあってディスクにないデータはない
//! - write-back(`write_back`):メモリにだけ書いて「汚れた」印を付け、`flush` のときと、そのセクタを
//!   捨てるときにディスクに書く。同じセクタへの何度もの書き込みが 1 回になるが、`flush` の前に
//!   電源が切れると消える
//!
//! いっぱいになったら、一番長く使われていないセクタを捨てる(LRU)。

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use hal::block::{BLOCK_SIZE, BlockDevice, BlockError};

struct Entry {
    data: Box<[u8; BLOCK_SIZE]>,
    /// 最後に使った順番(大きいほど最近)
    used: u64,
    /// まだディスクに書いていない(write-back)
    dirty: bool,
}

pub struct CachedDevice {
    inner: Box<dyn BlockDevice>,
    capacity: usize,
    entries: BTreeMap<u64, Entry>,
    /// 使った順番 → セクタ。先頭が一番長く使われていないもの(捨てるものを全部見ずに選べる)
    order: BTreeMap<u64, u64>,
    clock: u64,
    write_back: bool,
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
            write_back: false,
        }
    }

    /// write-back のキャッシュ(書き込みは `flush` か、そのセクタを捨てるときにディスクへ)
    pub fn write_back(inner: Box<dyn BlockDevice>, capacity: usize) -> Self {
        Self {
            write_back: true,
            ..Self::new(inner, capacity)
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

    /// セクタを覚える。いっぱいなら、一番長く使われていないものを捨てる(汚れていれば書いてから)
    fn remember(
        &mut self,
        lba: u64,
        data: &[u8; BLOCK_SIZE],
        dirty: bool,
    ) -> Result<(), BlockError> {
        if let Some(e) = self.entries.get_mut(&lba) {
            e.data.copy_from_slice(data);
            e.dirty |= dirty;
            self.touch(lba);
            return Ok(());
        }
        // 捨てるもののバッファを使い回す(セクタごとに確保し直さない)
        let buffer = if self.entries.len() >= self.capacity {
            let (_, oldest) = self.order.pop_first().expect("cache is full");
            let e = self.entries.remove(&oldest).expect("listed in order");
            if e.dirty {
                self.inner.write_block(oldest, &e.data)?;
            }
            Some(e.data)
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
                dirty,
            },
        );
        // 順番は 1 から数えるので、used: 0 は順番の表にない。touch で表に入る
        self.touch(lba);
        Ok(())
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
        self.remember(lba, buf, false)
    }

    fn write_block(&mut self, lba: u64, buf: &[u8; BLOCK_SIZE]) -> Result<(), BlockError> {
        if !self.write_back {
            self.inner.write_block(lba, buf)?;
        }
        self.remember(lba, buf, self.write_back)
    }

    /// 汚れたセクタを、番号の順にディスクへ書く
    fn flush(&mut self) -> Result<(), BlockError> {
        for (&lba, e) in self.entries.iter_mut() {
            if e.dirty {
                self.inner.write_block(lba, &e.data)?;
                e.dirty = false;
            }
        }
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::ramdisk::RamDisk;
    use std::boxed::Box;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 読んだ回数(と書いた回数)を数えるディスク
    struct Counting(RamDisk, Arc<AtomicUsize>);

    struct CountingWrites(RamDisk, Arc<AtomicUsize>);

    impl BlockDevice for CountingWrites {
        fn block_count(&self) -> u64 {
            self.0.block_count()
        }
        fn read_block(&mut self, lba: u64, buf: &mut [u8; BLOCK_SIZE]) -> Result<(), BlockError> {
            self.0.read_block(lba, buf)
        }
        fn write_block(&mut self, lba: u64, buf: &[u8; BLOCK_SIZE]) -> Result<(), BlockError> {
            self.1.fetch_add(1, Ordering::Relaxed);
            self.0.write_block(lba, buf)
        }
    }

    fn write_back(capacity: usize) -> (CachedDevice, Arc<AtomicUsize>) {
        let writes = Arc::new(AtomicUsize::new(0));
        let dev = CachedDevice::write_back(
            Box::new(CountingWrites(RamDisk::new(64), writes.clone())),
            capacity,
        );
        (dev, writes)
    }

    #[test]
    fn write_back_keeps_writes_in_memory_until_flush() {
        let (mut dev, writes) = write_back(8);
        dev.write_block(1, &[1; BLOCK_SIZE]).unwrap();
        dev.write_block(1, &[2; BLOCK_SIZE]).unwrap(); // 同じセクタへの 2 回目
        dev.write_block(2, &[3; BLOCK_SIZE]).unwrap();
        assert_eq!(writes.load(Ordering::Relaxed), 0);
        let mut buf = [0u8; BLOCK_SIZE];
        dev.read_block(1, &mut buf).unwrap();
        assert_eq!(buf, [2; BLOCK_SIZE]);
        dev.flush().unwrap();
        assert_eq!(
            writes.load(Ordering::Relaxed),
            2,
            "one write per dirty sector"
        );
        dev.inner.read_block(1, &mut buf).unwrap();
        assert_eq!(buf, [2; BLOCK_SIZE]);
        dev.flush().unwrap();
        assert_eq!(writes.load(Ordering::Relaxed), 2, "nothing left to write");
    }

    #[test]
    fn a_dirty_sector_is_written_before_it_is_dropped() {
        let (mut dev, writes) = write_back(1);
        dev.write_block(1, &[7; BLOCK_SIZE]).unwrap();
        let mut buf = [0u8; BLOCK_SIZE];
        dev.read_block(2, &mut buf).unwrap(); // いっぱい:汚れた 1 を書いてから捨てる
        assert_eq!(writes.load(Ordering::Relaxed), 1);
        dev.inner.read_block(1, &mut buf).unwrap();
        assert_eq!(buf, [7; BLOCK_SIZE]);
    }

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
