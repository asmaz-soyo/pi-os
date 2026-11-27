//! virtio-mmio(version 2)の共通部分。virtio-blk と virtio-net が使う。
//!
//! - レジスタの番地と状態のビット
//! - 初期化の前半([`negotiate`]):目印と種類を確かめ、リセットし、機能を取り決める
//! - キューの番地をデバイスに教える([`setup_queue`])
//! - ドライバ側の virtqueue([`Virtqueue`]):記述子を書く、avail リングに載せる、used リングから取り出す
//!
//! 1 本のキューは、連続した領域にこう並べる(大きさ 8 のとき):
//!
//! ```text
//!   0    記述子テーブル(16 バイト × 大きさ)
//!   128  avail リング(ドライバ → デバイス:「この記述子を処理して」)
//!   256  used リング(デバイス → ドライバ:「処理した。書いたのは何バイト」)
//! ```
#![no_std]

#[cfg(test)]
extern crate std;

use core::sync::atomic::{Ordering, fence};
use hal::mmio::Mmio;

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
pub const INTERRUPT_STATUS: usize = 0x060;
pub const INTERRUPT_ACK: usize = 0x064;
pub const STATUS: usize = 0x070;
pub const QUEUE_DESC_LOW: usize = 0x080;
pub const QUEUE_DESC_HIGH: usize = 0x084;
pub const QUEUE_DRIVER_LOW: usize = 0x090;
pub const QUEUE_DRIVER_HIGH: usize = 0x094;
pub const QUEUE_DEVICE_LOW: usize = 0x0a0;
pub const QUEUE_DEVICE_HIGH: usize = 0x0a4;
/// デバイス固有の設定(virtio-blk は容量、virtio-net は MAC アドレス)
pub const CONFIG: usize = 0x100;

pub const STATUS_ACK: u32 = 1;
pub const STATUS_DRIVER: u32 = 2;
pub const STATUS_DRIVER_OK: u32 = 4;
pub const STATUS_FEATURES_OK: u32 = 8;

/// "virt"
pub const MAGIC_VALUE: u32 = 0x7472_6976;
pub const DEVICE_NET: u32 = 1;
pub const DEVICE_BLOCK: u32 = 2;

/// 記述子のフラグ:鎖が続く、デバイスが書く
pub const DESC_NEXT: u16 = 1;
pub const DESC_WRITE: u16 = 2;

/// キューの領域の中の、avail リングと used リングの位置
pub const AVAIL: usize = 128;
pub const USED: usize = 256;

/// 初期化の前半。目印・版・種類を確かめてリセットし、機能を取り決める
/// (下位 32 ビットは `wanted_low` のうちデバイスが持つもの、上位は VIRTIO_F_VERSION_1 だけ)。
/// 返すのは (いまの状態, デバイスの下位 32 ビットの機能)。違うデバイス・断られたら None
pub fn negotiate<M: Mmio>(regs: &mut M, device: u32, wanted_low: u32) -> Option<(u32, u32)> {
    if regs.read(MAGIC) != MAGIC_VALUE || regs.read(VERSION) != 2 || regs.read(DEVICE_ID) != device
    {
        return None;
    }
    regs.write(STATUS, 0); // リセット
    regs.write(STATUS, STATUS_ACK); // デバイスがあるのに気づいた
    let mut status = STATUS_ACK | STATUS_DRIVER; // 動かし方を知っている
    regs.write(STATUS, status);
    regs.write(DEVICE_FEATURES_SEL, 0);
    let low = regs.read(DEVICE_FEATURES);
    regs.write(DRIVER_FEATURES_SEL, 0);
    regs.write(DRIVER_FEATURES, low & wanted_low);
    regs.write(DRIVER_FEATURES_SEL, 1);
    regs.write(DRIVER_FEATURES, 1); // VIRTIO_F_VERSION_1
    status |= STATUS_FEATURES_OK;
    regs.write(STATUS, status);
    // デバイスがこの組を受け入れたなら、FEATURES_OK が立ったまま読める
    if regs.read(STATUS) & STATUS_FEATURES_OK == 0 {
        return None;
    }
    Some((status, low))
}

/// キュー `q` を用意する:大きさと、`ring_pa` から並べた記述子・avail・used の番地を教える
pub fn setup_queue<M: Mmio>(regs: &mut M, q: u32, size: usize, ring_pa: u64) -> Option<()> {
    regs.write(QUEUE_SEL, q);
    if (regs.read(QUEUE_NUM_MAX) as usize) < size {
        return None;
    }
    regs.write(QUEUE_NUM, size as u32);
    for (low, off) in [
        (QUEUE_DESC_LOW, 0),
        (QUEUE_DRIVER_LOW, AVAIL),
        (QUEUE_DEVICE_LOW, USED),
    ] {
        let pa = ring_pa + off as u64;
        regs.write(low, pa as u32);
        regs.write(low + 4, (pa >> 32) as u32); // 上位はすぐ隣のレジスタ
    }
    regs.write(QUEUE_READY, 1);
    Some(())
}

/// 初期化の終わり:ドライバの準備ができたと知らせる
pub fn driver_ok<M: Mmio>(regs: &mut M, status: u32) {
    regs.write(STATUS, status | STATUS_DRIVER_OK);
}

/// 割り込みに応える(割り込みのハンドラから呼ぶ)。理由(1 = キューに使い終わったものがある、
/// 2 = 設定が変わった)を読み、同じ値を書いて消す。返すのは理由
pub fn ack_interrupt<M: Mmio>(regs: &mut M) -> u32 {
    let why = regs.read(INTERRUPT_STATUS);
    regs.write(INTERRUPT_ACK, why);
    why
}

/// ドライバ側の 1 本の virtqueue
pub struct Virtqueue {
    va: *mut u8,
    pa: u64,
    size: usize,
    /// 次に avail リングに載せる位置(数え続ける。16 ビットで一周)
    avail_idx: u16,
    /// 次に読む used リングの位置
    last_used: u16,
}

// SAFETY: 領域はこのキューだけが使う。共有するときはドライバごとロックで守る
unsafe impl Send for Virtqueue {}

impl Virtqueue {
    /// # Safety
    /// `va` から 512 バイトが書ける領域で、物理アドレスが `pa`。このキューだけが使うこと
    pub unsafe fn new(va: *mut u8, pa: u64, size: usize) -> Virtqueue {
        // SAFETY: 呼び出し元が領域の有効性を保証する
        unsafe { core::ptr::write_bytes(va, 0, 512) };
        Virtqueue {
            va,
            pa,
            size,
            avail_idx: 0,
            last_used: 0,
        }
    }

    /// 領域の物理アドレス(`setup_queue` に渡す)
    pub fn pa(&self) -> u64 {
        self.pa
    }

    fn put<const N: usize>(&mut self, off: usize, bytes: [u8; N]) {
        // SAFETY: off + N は領域(512 バイト)の中
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), self.va.add(off), N) };
    }

    fn get_u16(&self, off: usize) -> u16 {
        // SAFETY: 領域の中で 2 バイト境界。デバイスが書くので volatile で読む
        unsafe { core::ptr::read_volatile(self.va.add(off) as *const u16) }
    }

    fn get_u32(&self, off: usize) -> u32 {
        // SAFETY: 領域の中で 4 バイト境界
        unsafe { core::ptr::read_volatile(self.va.add(off) as *const u32) }
    }

    /// 記述子 `i` を書く:バッファの物理アドレス、長さ、フラグ、鎖の次
    pub fn desc(&mut self, i: usize, buf_pa: u64, len: u32, flags: u16, next: u16) {
        let base = i * 16;
        self.put(base, buf_pa.to_le_bytes());
        self.put(base + 8, len.to_le_bytes());
        self.put(base + 12, flags.to_le_bytes());
        self.put(base + 14, next.to_le_bytes());
    }

    /// 記述子 `id`(鎖の先頭)を avail リングに載せる(デバイスへの通知はしない)
    pub fn offer(&mut self, id: u16) {
        let slot = self.avail_idx as usize % self.size;
        self.put(AVAIL + 4 + slot * 2, id.to_le_bytes());
        // 中身を書いてから idx を進める(デバイスが、書きかけの場所を読まないように)
        fence(Ordering::SeqCst);
        self.avail_idx = self.avail_idx.wrapping_add(1);
        self.put(AVAIL + 2, self.avail_idx.to_le_bytes());
        fence(Ordering::SeqCst);
    }

    /// 載せたが、まだ返ってきていない数
    pub fn in_flight(&self) -> usize {
        self.avail_idx.wrapping_sub(self.last_used) as usize
    }

    /// 次に載せる avail リングの場所(送信のように、場所と記述子の番号を揃えて使うとき)
    pub fn next_slot(&self) -> usize {
        self.avail_idx as usize % self.size
    }

    /// デバイスが used リングに返したものを 1 つ取り出す:(記述子の番号, 書いた長さ)
    pub fn take_used(&mut self) -> Option<(usize, usize)> {
        fence(Ordering::SeqCst);
        if self.get_u16(USED + 2) == self.last_used {
            return None;
        }
        let slot = self.last_used as usize % self.size;
        self.last_used = self.last_used.wrapping_add(1);
        let id = self.get_u32(USED + 4 + slot * 8) as usize;
        let len = self.get_u32(USED + 8 + slot * 8) as usize;
        Some((id % self.size, len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hal::mock::FakeMmio;

    fn device(id: u32, features: u32) -> FakeMmio {
        let mut m = FakeMmio::new();
        m.queue_reads(MAGIC, &[MAGIC_VALUE]);
        m.queue_reads(VERSION, &[2]);
        m.queue_reads(DEVICE_ID, &[id]);
        m.queue_reads(DEVICE_FEATURES, &[features]);
        m.queue_reads(STATUS, &[STATUS_ACK | STATUS_DRIVER | STATUS_FEATURES_OK]);
        m
    }

    #[test]
    fn negotiate_resets_and_agrees_on_features_in_order() {
        let mut m = device(DEVICE_NET, 0x20 | 0x10000);
        assert_eq!(negotiate(&mut m, DEVICE_NET, 0x20), Some((0b1011, 0x10020)));
        assert_eq!(
            m.writes(),
            [
                (STATUS, 0), // リセット
                (STATUS, STATUS_ACK),
                (STATUS, STATUS_ACK | STATUS_DRIVER),
                (DEVICE_FEATURES_SEL, 0),
                (DRIVER_FEATURES_SEL, 0),
                (DRIVER_FEATURES, 0x20), // 欲しいもののうち、デバイスが持つもの
                (DRIVER_FEATURES_SEL, 1),
                (DRIVER_FEATURES, 1), // VIRTIO_F_VERSION_1(ビット 32)
                (STATUS, 0b1011),
            ]
        );
    }

    #[test]
    fn negotiate_rejects_other_devices_and_refused_features() {
        assert_eq!(negotiate(&mut device(DEVICE_BLOCK, 0), DEVICE_NET, 0), None);
        let mut old = FakeMmio::new();
        old.queue_reads(MAGIC, &[MAGIC_VALUE]);
        old.queue_reads(VERSION, &[1]); // 古い(legacy)virtio-mmio
        assert_eq!(negotiate(&mut old, DEVICE_NET, 0), None);
        // 状態を読み直すと 0(デバイスが FEATURES_OK を立てない = 機能の組を断った)
        let mut m = FakeMmio::new();
        m.queue_reads(MAGIC, &[MAGIC_VALUE]);
        m.queue_reads(VERSION, &[2]);
        m.queue_reads(DEVICE_ID, &[DEVICE_NET]);
        assert_eq!(negotiate(&mut m, DEVICE_NET, 0), None);
    }

    #[test]
    fn setup_queue_tells_the_device_where_the_rings_are() {
        let mut m = FakeMmio::new();
        m.queue_reads(QUEUE_NUM_MAX, &[256]);
        assert_eq!(setup_queue(&mut m, 1, 8, 0x1_2345_6000), Some(()));
        assert_eq!(
            m.writes(),
            [
                (QUEUE_SEL, 1),
                (QUEUE_NUM, 8),
                (QUEUE_DESC_LOW, 0x2345_6000),
                (QUEUE_DESC_HIGH, 1),
                (QUEUE_DRIVER_LOW, 0x2345_6080),
                (QUEUE_DRIVER_HIGH, 1),
                (QUEUE_DEVICE_LOW, 0x2345_6100),
                (QUEUE_DEVICE_HIGH, 1),
                (QUEUE_READY, 1),
            ]
        );
        // デバイスのキューが小さすぎる
        let mut small = FakeMmio::new();
        small.queue_reads(QUEUE_NUM_MAX, &[4]);
        assert_eq!(setup_queue(&mut small, 0, 8, 0), None);
    }

    #[test]
    fn ack_interrupt_reads_the_reason_and_writes_it_back() {
        let mut m = FakeMmio::new();
        m.queue_reads(INTERRUPT_STATUS, &[1]);
        assert_eq!(ack_interrupt(&mut m), 1);
        assert_eq!(m.writes(), [(INTERRUPT_ACK, 1)]);
        let mut done = FakeMmio::new();
        driver_ok(&mut done, 0b1011);
        assert_eq!(done.writes(), [(STATUS, 0b1111)]);
    }

    #[repr(align(4096))]
    struct Mem([u8; 4096]);

    fn u16_at(m: &Mem, off: usize) -> u16 {
        u16::from_le_bytes(m.0[off..off + 2].try_into().unwrap())
    }

    #[test]
    fn the_queue_offers_descriptors_and_takes_back_used_ones() {
        let mut mem = std::boxed::Box::new(Mem([0; 4096]));
        let va = mem.0.as_mut_ptr();
        let mut q = unsafe { Virtqueue::new(va, 0x8000_0000, 8) };
        assert_eq!(q.pa(), 0x8000_0000);
        q.desc(3, 0x9000_0000, 2048, DESC_WRITE, 0);
        assert_eq!(&mem.0[48..56], &0x9000_0000u64.to_le_bytes());
        assert_eq!(&mem.0[56..60], &2048u32.to_le_bytes());
        assert_eq!(u16_at(&mem, 60), DESC_WRITE);
        // 10 回載せる:avail の idx は数え続け、場所は大きさ(8)で一周する
        for id in 0..10u16 {
            q.offer(id);
        }
        assert_eq!(u16_at(&mem, AVAIL + 2), 10);
        assert_eq!(
            u16_at(&mem, AVAIL + 4),
            8,
            "場所 0 には 9 回目(id 8)が上書きされている"
        );
        assert_eq!(u16_at(&mem, AVAIL + 4 + 2), 9);
        assert_eq!(u16_at(&mem, AVAIL + 4 + 2 * 2), 2);
        // デバイスはまだ何も返していない
        assert_eq!(q.take_used(), None);
        // デバイスのふり:used の場所 0 に (id 3, 1514 バイト) を書いて idx を 1 に
        mem.0[USED + 4..USED + 8].copy_from_slice(&3u32.to_le_bytes());
        mem.0[USED + 8..USED + 12].copy_from_slice(&1514u32.to_le_bytes());
        mem.0[USED + 2..USED + 4].copy_from_slice(&1u16.to_le_bytes());
        assert_eq!(q.take_used(), Some((3, 1514)));
        assert_eq!(q.take_used(), None);
    }

    #[test]
    fn in_flight_counts_what_the_device_has_not_returned() {
        let mut mem = std::boxed::Box::new(Mem([0; 4096]));
        let mut q = unsafe { Virtqueue::new(mem.0.as_mut_ptr(), 0, 8) };
        assert_eq!((q.in_flight(), q.next_slot()), (0, 0));
        q.offer(0);
        q.offer(1);
        assert_eq!((q.in_flight(), q.next_slot()), (2, 2));
        mem.0[USED + 2..USED + 4].copy_from_slice(&1u16.to_le_bytes());
        q.take_used();
        assert_eq!(q.in_flight(), 1);
    }
}
