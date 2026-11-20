//! virtio-blk(virtio-mmio version 2)のドライバ。QEMU virt のディスク。
//!
//! virtio は「仮想マシンのための標準的なデバイス」。ドライバとデバイスは、共有メモリの上の
//! 「virtqueue」(記述子テーブル、avail リング、used リング)でやり取りする。
//!
//! ```text
//! 1 ページ(4KiB)の DMA 領域
//!   0      記述子テーブル(16 バイト × QUEUE_SIZE)
//!   128    avail リング(ドライバ → デバイス:「この記述子を処理して」)
//!   256    used リング(デバイス → ドライバ:「処理した」)
//!   512    要求のヘッダ(種類、セクタ)
//!   528    状態(デバイスが 0 = 成功を書く)
//!   1024   データ(512 バイト)
//! ```
//!
//! 今は割り込みを使わず、used リングを見て待つ(ポーリング)。
#![no_std]

#[cfg(test)]
extern crate std;

use core::sync::atomic::{Ordering, fence};
use hal::block::{BLOCK_SIZE, BlockDevice, BlockError};
use hal::mmio::Mmio;

// レジスタ(virtio-mmio version 2)
pub const MAGIC: usize = 0x000;
pub const VERSION: usize = 0x004;
pub const DEVICE_ID: usize = 0x008;
pub const DEVICE_FEATURES: usize = 0x010;
pub const DEVICE_FEATURES_SEL: usize = 0x014;
pub const DRIVER_FEATURES: usize = 0x020;
pub const DRIVER_FEATURES_SEL: usize = 0x024;
pub const QUEUE_SEL: usize = 0x030;
pub const QUEUE_NUM_MAX: usize = 0x034;
pub const QUEUE_NUM: usize = 0x038;
pub const QUEUE_READY: usize = 0x044;
pub const QUEUE_NOTIFY: usize = 0x050;
pub const STATUS: usize = 0x070;
pub const QUEUE_DESC_LOW: usize = 0x080;
pub const QUEUE_DESC_HIGH: usize = 0x084;
pub const QUEUE_DRIVER_LOW: usize = 0x090;
pub const QUEUE_DRIVER_HIGH: usize = 0x094;
pub const QUEUE_DEVICE_LOW: usize = 0x0a0;
pub const QUEUE_DEVICE_HIGH: usize = 0x0a4;
/// デバイス固有の設定(virtio-blk では容量 = セクタ数 u64)
pub const CONFIG: usize = 0x100;

pub const STATUS_ACK: u32 = 1;
pub const STATUS_DRIVER: u32 = 2;
pub const STATUS_DRIVER_OK: u32 = 4;
pub const STATUS_FEATURES_OK: u32 = 8;

const DEVICE_ID_BLOCK: u32 = 2;
const MAGIC_VALUE: u32 = 0x7472_6976; // "virt"

pub const QUEUE_SIZE: usize = 8;
pub const AVAIL_OFFSET: usize = 128;
pub const USED_OFFSET: usize = 256;
pub const HEADER_OFFSET: usize = 512;
pub const STATUS_OFFSET: usize = 528;
pub const DATA_OFFSET: usize = 1024;

pub const DESC_NEXT: u16 = 1;
pub const DESC_WRITE: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    Read,
    Write,
}

pub struct VirtioBlk<M: Mmio> {
    regs: M,
    dma: *mut u8,
    dma_pa: u64,
    capacity: u64,
    avail_idx: u16,
    last_used: u16,
}

// SAFETY: DMA 領域はこのドライバだけが使う。共有するときはロックで守る
unsafe impl<M: Mmio + Send> Send for VirtioBlk<M> {}

impl<M: Mmio> VirtioBlk<M> {
    /// デバイスを初期化する。virtio-blk でなければ `None`。
    ///
    /// # Safety
    /// `dma` は 4KiB の書ける領域で、物理アドレスが `dma_pa`。このドライバだけが使うこと。
    pub unsafe fn new(mut regs: M, dma: *mut u8, dma_pa: u64) -> Option<Self> {
        if regs.read(MAGIC) != MAGIC_VALUE || regs.read(VERSION) != 2 {
            return None;
        }
        if regs.read(DEVICE_ID) != DEVICE_ID_BLOCK {
            return None;
        }
        // SAFETY: 呼び出し元が領域の有効性を保証する
        unsafe { core::ptr::write_bytes(dma, 0, 4096) };
        regs.write(STATUS, 0); // リセット
        let mut status = STATUS_ACK | STATUS_DRIVER;
        regs.write(STATUS, STATUS_ACK);
        regs.write(STATUS, status);
        // 機能の取り決め:VIRTIO_F_VERSION_1(ビット 32)だけを受け入れる
        regs.write(DEVICE_FEATURES_SEL, 1);
        let _ = regs.read(DEVICE_FEATURES);
        regs.write(DRIVER_FEATURES_SEL, 0);
        regs.write(DRIVER_FEATURES, 0);
        regs.write(DRIVER_FEATURES_SEL, 1);
        regs.write(DRIVER_FEATURES, 1);
        status |= STATUS_FEATURES_OK;
        regs.write(STATUS, status);
        if regs.read(STATUS) & STATUS_FEATURES_OK == 0 {
            return None;
        }
        // virtqueue 0 を用意する
        regs.write(QUEUE_SEL, 0);
        if (regs.read(QUEUE_NUM_MAX) as usize) < QUEUE_SIZE {
            return None;
        }
        regs.write(QUEUE_NUM, QUEUE_SIZE as u32);
        let addr = |off: usize| dma_pa + off as u64;
        regs.write(QUEUE_DESC_LOW, addr(0) as u32);
        regs.write(QUEUE_DESC_HIGH, (addr(0) >> 32) as u32);
        regs.write(QUEUE_DRIVER_LOW, addr(AVAIL_OFFSET) as u32);
        regs.write(QUEUE_DRIVER_HIGH, (addr(AVAIL_OFFSET) >> 32) as u32);
        regs.write(QUEUE_DEVICE_LOW, addr(USED_OFFSET) as u32);
        regs.write(QUEUE_DEVICE_HIGH, (addr(USED_OFFSET) >> 32) as u32);
        regs.write(QUEUE_READY, 1);
        let low = regs.read(CONFIG) as u64;
        let high = regs.read(CONFIG + 4) as u64;
        regs.write(STATUS, status | STATUS_DRIVER_OK);
        Some(Self {
            regs,
            dma,
            dma_pa,
            capacity: (high << 32) | low,
            avail_idx: 0,
            last_used: 0,
        })
    }

    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    pub fn regs(&self) -> &M {
        &self.regs
    }

    fn put<const N: usize>(&mut self, off: usize, bytes: [u8; N]) {
        // SAFETY: off + N は DMA 領域(4KiB)の中
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), self.dma.add(off), N) };
    }

    fn get_u8(&self, off: usize) -> u8 {
        // SAFETY: DMA 領域の中。デバイスが書くので volatile で読む
        unsafe { core::ptr::read_volatile(self.dma.add(off)) }
    }

    fn get_u16(&self, off: usize) -> u16 {
        // SAFETY: DMA 領域の中で 2 バイト境界
        unsafe { core::ptr::read_volatile(self.dma.add(off) as *const u16) }
    }

    /// 記述子 `i` を書く
    fn desc(&mut self, i: usize, off: usize, len: u32, flags: u16, next: u16) {
        let base = i * 16;
        self.put(base, (self.dma_pa + off as u64).to_le_bytes());
        self.put(base + 8, len.to_le_bytes());
        self.put(base + 12, flags.to_le_bytes());
        self.put(base + 14, next.to_le_bytes());
    }

    /// 要求を組み立てて、デバイスに知らせる(完了は `poll` で確かめる)。
    pub fn start(&mut self, kind: RequestKind, lba: u64, data: Option<&[u8; BLOCK_SIZE]>) {
        let ty: u32 = match kind {
            RequestKind::Read => 0,  // VIRTIO_BLK_T_IN
            RequestKind::Write => 1, // VIRTIO_BLK_T_OUT
        };
        self.put(HEADER_OFFSET, ty.to_le_bytes());
        self.put(HEADER_OFFSET + 4, 0u32.to_le_bytes());
        self.put(HEADER_OFFSET + 8, lba.to_le_bytes());
        self.put(STATUS_OFFSET, [0xffu8]);
        if let Some(d) = data {
            self.put(DATA_OFFSET, *d);
        }
        let data_flags = if kind == RequestKind::Read {
            DESC_NEXT | DESC_WRITE
        } else {
            DESC_NEXT
        };
        self.desc(0, HEADER_OFFSET, 16, DESC_NEXT, 1);
        self.desc(1, DATA_OFFSET, BLOCK_SIZE as u32, data_flags, 2);
        self.desc(2, STATUS_OFFSET, 1, DESC_WRITE, 0);
        // avail リングに記述子 0(鎖の先頭)を載せてから idx を進める
        let slot = self.avail_idx as usize % QUEUE_SIZE;
        self.put(AVAIL_OFFSET + 4 + slot * 2, 0u16.to_le_bytes());
        fence(Ordering::SeqCst);
        self.avail_idx = self.avail_idx.wrapping_add(1);
        self.put(AVAIL_OFFSET + 2, self.avail_idx.to_le_bytes());
        fence(Ordering::SeqCst);
        self.regs.write(QUEUE_NOTIFY, 0);
    }

    /// 要求が終わっていれば結果を返す。まだなら `None`。
    pub fn poll(&mut self) -> Option<Result<(), BlockError>> {
        fence(Ordering::SeqCst);
        if self.get_u16(USED_OFFSET + 2) == self.last_used {
            return None;
        }
        self.last_used = self.last_used.wrapping_add(1);
        fence(Ordering::SeqCst);
        Some(if self.get_u8(STATUS_OFFSET) == 0 {
            Ok(())
        } else {
            Err(BlockError::Io)
        })
    }

    /// 読んだデータを取り出す
    pub fn take_data(&self, buf: &mut [u8; BLOCK_SIZE]) {
        // SAFETY: DATA_OFFSET から 512 バイトは DMA 領域の中
        unsafe {
            core::ptr::copy_nonoverlapping(self.dma.add(DATA_OFFSET), buf.as_mut_ptr(), BLOCK_SIZE)
        };
    }

    fn wait(&mut self) -> Result<(), BlockError> {
        loop {
            if let Some(r) = self.poll() {
                return r;
            }
            core::hint::spin_loop();
        }
    }
}

impl<M: Mmio + Send> BlockDevice for VirtioBlk<M> {
    fn block_count(&self) -> u64 {
        self.capacity
    }

    fn read_block(&mut self, lba: u64, buf: &mut [u8; BLOCK_SIZE]) -> Result<(), BlockError> {
        if lba >= self.capacity {
            return Err(BlockError::OutOfRange);
        }
        self.start(RequestKind::Read, lba, None);
        self.wait()?;
        self.take_data(buf);
        Ok(())
    }

    fn write_block(&mut self, lba: u64, buf: &[u8; BLOCK_SIZE]) -> Result<(), BlockError> {
        if lba >= self.capacity {
            return Err(BlockError::OutOfRange);
        }
        self.start(RequestKind::Write, lba, Some(buf));
        self.wait()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hal::mock::FakeMmio;
    use std::boxed::Box;

    /// DMA に使う 4KiB(物理アドレスは 0x8000_0000 に見立てる)
    #[repr(align(4096))]
    struct Page([u8; 4096]);
    const PA: u64 = 0x8000_0000;

    fn device() -> (VirtioBlk<FakeMmio>, &'static mut Page) {
        let mut mmio = FakeMmio::new();
        mmio.queue_reads(MAGIC, &[0x7472_6976]);
        mmio.queue_reads(VERSION, &[2]);
        mmio.queue_reads(DEVICE_ID, &[2]);
        mmio.queue_reads(QUEUE_NUM_MAX, &[16]);
        mmio.queue_reads(STATUS, &[STATUS_ACK | STATUS_DRIVER | STATUS_FEATURES_OK]);
        mmio.queue_reads(CONFIG, &[2048, 0]); // 容量 2048 セクタ(下位、上位)
        let page = Box::leak(Box::new(Page([0; 4096])));
        let dev = unsafe { VirtioBlk::new(mmio, page.0.as_mut_ptr(), PA) }.unwrap();
        (dev, page)
    }

    #[test]
    fn init_negotiates_version_1_and_sets_up_queue_0() {
        let (dev, _) = device();
        let w = dev.regs().writes();
        assert_eq!(w[0], (STATUS, 0), "reset first");
        assert!(
            w.contains(&(DRIVER_FEATURES_SEL, 1)) && w.contains(&(DRIVER_FEATURES, 1)),
            "VERSION_1"
        );
        assert!(w.contains(&(QUEUE_NUM, QUEUE_SIZE as u32)));
        assert!(w.contains(&(QUEUE_DESC_LOW, PA as u32)));
        assert!(w.contains(&(QUEUE_DRIVER_LOW, (PA + AVAIL_OFFSET as u64) as u32)));
        assert!(w.contains(&(QUEUE_DEVICE_LOW, (PA + USED_OFFSET as u64) as u32)));
        assert!(w.contains(&(QUEUE_READY, 1)));
        assert_eq!(
            *w.last().unwrap(),
            (
                STATUS,
                STATUS_ACK | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK
            )
        );
        assert_eq!(dev.capacity(), 2048);
    }

    #[test]
    fn not_a_virtio_block_device_is_rejected() {
        let mut mmio = FakeMmio::new();
        mmio.queue_reads(MAGIC, &[0x7472_6976]);
        mmio.queue_reads(VERSION, &[2]);
        mmio.queue_reads(DEVICE_ID, &[1]); // ネットワーク
        let page = Box::leak(Box::new(Page([0; 4096])));
        assert!(unsafe { VirtioBlk::new(mmio, page.0.as_mut_ptr(), PA) }.is_none());
    }

    fn u16_at(p: &Page, off: usize) -> u16 {
        u16::from_le_bytes(p.0[off..off + 2].try_into().unwrap())
    }
    fn u64_at(p: &Page, off: usize) -> u64 {
        u64::from_le_bytes(p.0[off..off + 8].try_into().unwrap())
    }

    #[test]
    fn a_read_request_is_a_chain_of_header_data_and_status() {
        let (mut dev, page) = device();
        dev.start(RequestKind::Read, 7, None);
        // ヘッダ:種類 0(読む)、セクタ 7
        assert_eq!(page.0[HEADER_OFFSET], 0);
        assert_eq!(u64_at(page, HEADER_OFFSET + 8), 7);
        // 記述子 0 → 1 → 2(NEXT)、データはデバイスが書く(WRITE)
        assert_eq!(u64_at(page, 0), PA + HEADER_OFFSET as u64);
        assert_eq!(u16_at(page, 12), DESC_NEXT);
        assert_eq!(u64_at(page, 16), PA + DATA_OFFSET as u64);
        assert_eq!(u16_at(page, 16 + 12), DESC_NEXT | DESC_WRITE);
        assert_eq!(u64_at(page, 32), PA + STATUS_OFFSET as u64);
        assert_eq!(u16_at(page, 32 + 12), DESC_WRITE);
        // avail リングに記述子 0 を載せ、idx を進め、通知する
        assert_eq!(u16_at(page, AVAIL_OFFSET + 2), 1);
        assert_eq!(u16_at(page, AVAIL_OFFSET + 4), 0);
        assert_eq!(dev.regs().writes().last(), Some(&(QUEUE_NOTIFY, 0)));
    }

    #[test]
    fn poll_completes_when_the_device_advances_the_used_ring() {
        let (mut dev, page) = device();
        dev.start(RequestKind::Read, 0, None);
        assert_eq!(dev.poll(), None, "device has not answered yet");
        // デバイスのふり:データを書き、状態 0(成功)、used の idx を 1 にする
        page.0[DATA_OFFSET..DATA_OFFSET + 4].copy_from_slice(b"DISK");
        page.0[STATUS_OFFSET] = 0;
        page.0[USED_OFFSET + 2] = 1;
        let mut buf = [0u8; 512];
        assert_eq!(dev.poll(), Some(Ok(())));
        dev.take_data(&mut buf);
        assert_eq!(&buf[..4], b"DISK");
    }

    #[test]
    fn a_failed_status_is_an_io_error() {
        let (mut dev, page) = device();
        dev.start(RequestKind::Write, 3, Some(&[1u8; 512]));
        assert_eq!(page.0[HEADER_OFFSET], 1, "type OUT");
        assert_eq!(&page.0[DATA_OFFSET..DATA_OFFSET + 2], &[1, 1]);
        page.0[STATUS_OFFSET] = 1; // VIRTIO_BLK_S_IOERR
        page.0[USED_OFFSET + 2] = 1;
        assert_eq!(dev.poll(), Some(Err(hal::block::BlockError::Io)));
    }
}
