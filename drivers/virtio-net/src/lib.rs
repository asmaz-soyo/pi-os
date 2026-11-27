//! virtio-net(virtio-mmio version 2)のドライバ。QEMU virt のネットワークカード。
//!
//! virtqueue は 2 本:0 番が受信(デバイス → ドライバ)、1 番が送信(ドライバ → デバイス)。
//! どちらも、フレームの前に 12 バイトの `virtio_net_hdr` が付く(チェックサムの肩代わりなどの
//! 情報。今は使わないので 0)。
//!
//! ```text
//! リングのページ(4KiB)        バッファのページ i(QUEUE_SIZE 枚)
//!   0     受信の記述子           0     受信のバッファ i(2KiB)
//!   128   受信の avail           2048  送信のバッファ i(2KiB)
//!   256   受信の used
//!   512   送信の記述子
//!   640   送信の avail
//!   768   送信の used
//! ```
//!
//! 受信:最初にすべての受信バッファをデバイスに渡しておく。フレームが届くと、デバイスが
//! used リングに「どのバッファに何バイト」を書く。読んだら、同じバッファをまた渡す。
//! 送信:空いている送信バッファにフレームを写し、avail リングに載せて知らせる。
//! 今は割り込みを使わず、used リングを見に行く(ポーリング)。
#![no_std]

#[cfg(test)]
extern crate std;

use core::sync::atomic::{Ordering, fence};
use hal::mmio::Mmio;
use hal::net::{LinkType, NetDev, NetError};

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
pub const INTERRUPT_STATUS: usize = 0x060;
pub const INTERRUPT_ACK: usize = 0x064;
pub const STATUS: usize = 0x070;
pub const QUEUE_DESC_LOW: usize = 0x080;
pub const QUEUE_DESC_HIGH: usize = 0x084;
pub const QUEUE_DRIVER_LOW: usize = 0x090;
pub const QUEUE_DRIVER_HIGH: usize = 0x094;
pub const QUEUE_DEVICE_LOW: usize = 0x0a0;
pub const QUEUE_DEVICE_HIGH: usize = 0x0a4;
/// デバイス固有の設定(virtio-net では先頭 6 バイトが MAC アドレス)
pub const CONFIG: usize = 0x100;

pub const STATUS_ACK: u32 = 1;
pub const STATUS_DRIVER: u32 = 2;
pub const STATUS_DRIVER_OK: u32 = 4;
pub const STATUS_FEATURES_OK: u32 = 8;

const DEVICE_ID_NET: u32 = 1;
const MAGIC_VALUE: u32 = 0x7472_6976; // "virt"
/// 機能のビット:MAC アドレスを設定から読める(下位 32 ビットの 5)
const F_MAC: u32 = 1 << 5;

pub const QUEUE_SIZE: usize = 8;
pub const RX: u32 = 0;
pub const TX: u32 = 1;
/// キューごとの、リングのページの中の位置
/// 割り込みに応える(割り込みのハンドラから呼ぶ)。理由(1 = キューに使い終わったものがある、
/// 2 = 設定が変わった)を読み、同じ値を書いて消す。返すのは理由
pub fn ack_interrupt<M: Mmio>(regs: &mut M) -> u32 {
    let why = regs.read(INTERRUPT_STATUS);
    regs.write(INTERRUPT_ACK, why);
    why
}

pub const fn queue_base(q: u32) -> usize {
    q as usize * 512
}
pub const AVAIL: usize = 128;
pub const USED: usize = 256;
pub const DESC_WRITE: u16 = 2;
/// virtio_net_hdr の大きさ(VERSION_1 では num_buffers まで入れて 12 バイト)
pub const HDR_LEN: usize = 12;
/// 1 つのバッファの大きさ
pub const BUF_LEN: usize = 2048;
/// Ethernet の MTU
pub const MTU: usize = 1500;

/// DMA に使うページ(仮想アドレスと物理アドレス)
#[derive(Clone, Copy)]
pub struct Page {
    pub va: *mut u8,
    pub pa: u64,
}

/// 1 本の virtqueue の、ドライバ側の数え方
#[derive(Default)]
struct Queue {
    /// 次に avail リングに載せる位置(数え続ける)
    avail_idx: u16,
    /// 次に読む used リングの位置(数え続ける)
    last_used: u16,
}

pub struct VirtioNet<M: Mmio> {
    regs: M,
    ring: Page,
    bufs: [Page; QUEUE_SIZE],
    mac: [u8; 6],
    rx: Queue,
    tx: Queue,
}

// SAFETY: DMA のページはこのドライバだけが使う。共有するときはロックで守る
unsafe impl<M: Mmio + Send> Send for VirtioNet<M> {}

impl<M: Mmio> VirtioNet<M> {
    /// デバイスを初期化する。virtio-net でなければ `None`。
    ///
    /// # Safety
    /// `ring` と `bufs` はそれぞれ 4KiB の書ける領域で、物理アドレスが `pa`。このドライバだけが使うこと。
    pub unsafe fn new(mut regs: M, ring: Page, bufs: [Page; QUEUE_SIZE]) -> Option<Self> {
        if regs.read(MAGIC) != MAGIC_VALUE || regs.read(VERSION) != 2 {
            return None;
        }
        if regs.read(DEVICE_ID) != DEVICE_ID_NET {
            return None;
        }
        // SAFETY: 呼び出し元が領域の有効性を保証する
        unsafe { core::ptr::write_bytes(ring.va, 0, 4096) };
        regs.write(STATUS, 0); // リセット
        let mut status = STATUS_ACK | STATUS_DRIVER;
        regs.write(STATUS, STATUS_ACK);
        regs.write(STATUS, status);
        // 機能の取り決め:MAC(下位のビット 5)と VERSION_1(ビット 32)
        regs.write(DEVICE_FEATURES_SEL, 0);
        let low = regs.read(DEVICE_FEATURES);
        regs.write(DRIVER_FEATURES_SEL, 0);
        regs.write(DRIVER_FEATURES, low & F_MAC);
        regs.write(DRIVER_FEATURES_SEL, 1);
        regs.write(DRIVER_FEATURES, 1);
        status |= STATUS_FEATURES_OK;
        regs.write(STATUS, status);
        if regs.read(STATUS) & STATUS_FEATURES_OK == 0 {
            return None;
        }
        for q in [RX, TX] {
            regs.write(QUEUE_SEL, q);
            if (regs.read(QUEUE_NUM_MAX) as usize) < QUEUE_SIZE {
                return None;
            }
            regs.write(QUEUE_NUM, QUEUE_SIZE as u32);
            let addr = |off: usize| ring.pa + (queue_base(q) + off) as u64;
            regs.write(QUEUE_DESC_LOW, addr(0) as u32);
            regs.write(QUEUE_DESC_HIGH, (addr(0) >> 32) as u32);
            regs.write(QUEUE_DRIVER_LOW, addr(AVAIL) as u32);
            regs.write(QUEUE_DRIVER_HIGH, (addr(AVAIL) >> 32) as u32);
            regs.write(QUEUE_DEVICE_LOW, addr(USED) as u32);
            regs.write(QUEUE_DEVICE_HIGH, (addr(USED) >> 32) as u32);
            regs.write(QUEUE_READY, 1);
        }
        let mut mac = [0u8; 6];
        if low & F_MAC != 0 {
            let a = regs.read(CONFIG).to_le_bytes();
            let b = regs.read(CONFIG + 4).to_le_bytes();
            mac = [a[0], a[1], a[2], a[3], b[0], b[1]];
        }
        regs.write(STATUS, status | STATUS_DRIVER_OK);
        let mut dev = Self {
            regs,
            ring,
            bufs,
            mac,
            rx: Queue::default(),
            tx: Queue::default(),
        };
        // 受信のバッファを全部デバイスに渡す
        for i in 0..QUEUE_SIZE {
            dev.desc(RX, i, dev.bufs[i].pa, BUF_LEN as u32, DESC_WRITE);
            dev.offer(RX, i as u16);
        }
        dev.regs.write(QUEUE_NOTIFY, RX);
        Some(dev)
    }

    pub fn regs(&self) -> &M {
        &self.regs
    }

    fn ring_put<const N: usize>(&mut self, off: usize, bytes: [u8; N]) {
        // SAFETY: off + N はリングのページ(4KiB)の中
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), self.ring.va.add(off), N) };
    }

    fn ring_u16(&self, off: usize) -> u16 {
        // SAFETY: リングのページの中で 2 バイト境界。デバイスが書くので volatile で読む
        unsafe { core::ptr::read_volatile(self.ring.va.add(off) as *const u16) }
    }

    fn ring_u32(&self, off: usize) -> u32 {
        // SAFETY: リングのページの中で 4 バイト境界
        unsafe { core::ptr::read_volatile(self.ring.va.add(off) as *const u32) }
    }

    /// キュー `q` の記述子 `i` を書く(1 つで完結。次はない)
    fn desc(&mut self, q: u32, i: usize, pa: u64, len: u32, flags: u16) {
        let base = queue_base(q) + i * 16;
        self.ring_put(base, pa.to_le_bytes());
        self.ring_put(base + 8, len.to_le_bytes());
        self.ring_put(base + 12, flags.to_le_bytes());
        self.ring_put(base + 14, 0u16.to_le_bytes());
    }

    /// 記述子 `id` を avail リングに載せる(通知はしない)
    fn offer(&mut self, q: u32, id: u16) {
        let base = queue_base(q) + AVAIL;
        let queue = if q == RX { &mut self.rx } else { &mut self.tx };
        let slot = queue.avail_idx as usize % QUEUE_SIZE;
        queue.avail_idx = queue.avail_idx.wrapping_add(1);
        let idx = queue.avail_idx;
        self.ring_put(base + 4 + slot * 2, id.to_le_bytes());
        fence(Ordering::SeqCst);
        self.ring_put(base + 2, idx.to_le_bytes());
        fence(Ordering::SeqCst);
    }

    /// デバイスが used リングに返したものを 1 つ取り出す:(記述子の番号, 長さ)
    fn take_used(&mut self, q: u32) -> Option<(usize, usize)> {
        let base = queue_base(q) + USED;
        fence(Ordering::SeqCst);
        let device_idx = self.ring_u16(base + 2);
        let queue = if q == RX { &mut self.rx } else { &mut self.tx };
        if device_idx == queue.last_used {
            return None;
        }
        let slot = queue.last_used as usize % QUEUE_SIZE;
        queue.last_used = queue.last_used.wrapping_add(1);
        let id = self.ring_u32(base + 4 + slot * 8) as usize;
        let len = self.ring_u32(base + 8 + slot * 8) as usize;
        Some((id % QUEUE_SIZE, len))
    }

    /// 送り終わった送信バッファを数え直す
    fn reclaim_tx(&mut self) {
        while self.take_used(TX).is_some() {}
    }
}

impl<M: Mmio + Send> NetDev for VirtioNet<M> {
    fn link_type(&self) -> LinkType {
        LinkType::Ethernet
    }

    fn mac(&self) -> [u8; 6] {
        self.mac
    }

    fn mtu(&self) -> usize {
        MTU
    }

    fn send(&mut self, frame: &[u8]) -> Result<(), NetError> {
        if HDR_LEN + frame.len() > BUF_LEN {
            return Err(NetError::TooLarge);
        }
        self.reclaim_tx();
        let in_flight = self.tx.avail_idx.wrapping_sub(self.tx.last_used) as usize;
        if in_flight >= QUEUE_SIZE {
            return Err(NetError::Busy);
        }
        let i = self.tx.avail_idx as usize % QUEUE_SIZE;
        let buf = self.bufs[i];
        // SAFETY: 送信のバッファはページの後ろ半分(2KiB)で、大きさは確かめた
        unsafe {
            let dst = buf.va.add(BUF_LEN);
            core::ptr::write_bytes(dst, 0, HDR_LEN);
            core::ptr::copy_nonoverlapping(frame.as_ptr(), dst.add(HDR_LEN), frame.len());
        }
        self.desc(
            TX,
            i,
            buf.pa + BUF_LEN as u64,
            (HDR_LEN + frame.len()) as u32,
            0,
        );
        self.offer(TX, i as u16);
        self.regs.write(QUEUE_NOTIFY, TX);
        Ok(())
    }

    fn recv(&mut self, out: &mut [u8]) -> Result<Option<usize>, NetError> {
        let Some((id, len)) = self.take_used(RX) else {
            return Ok(None);
        };
        let n = len.saturating_sub(HDR_LEN).min(out.len());
        // SAFETY: 受信のバッファはページの前半(2KiB)で、len はデバイスが書いた長さ(BUF_LEN 以下)
        unsafe {
            core::ptr::copy_nonoverlapping(self.bufs[id].va.add(HDR_LEN), out.as_mut_ptr(), n)
        };
        // 同じバッファをまたデバイスに渡す
        self.offer(RX, id as u16);
        self.regs.write(QUEUE_NOTIFY, RX);
        Ok(Some(n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hal::mock::FakeMmio;
    use std::boxed::Box;

    #[repr(align(4096))]
    struct Mem([u8; 4096]);

    /// リングのページは物理 0x8000_0000、バッファのページ i は 0x8000_0000 + (i + 1) * 4096 に見立てる
    const PA: u64 = 0x8000_0000;

    struct Fake {
        dev: VirtioNet<FakeMmio>,
        ring: &'static mut Mem,
        bufs: std::vec::Vec<*mut u8>,
    }

    fn mmio(features: u32) -> FakeMmio {
        let mut m = FakeMmio::new();
        m.queue_reads(MAGIC, &[0x7472_6976]);
        m.queue_reads(VERSION, &[2]);
        m.queue_reads(DEVICE_ID, &[1]);
        m.queue_reads(DEVICE_FEATURES, &[features]);
        m.queue_reads(STATUS, &[STATUS_ACK | STATUS_DRIVER | STATUS_FEATURES_OK]);
        m.queue_reads(QUEUE_NUM_MAX, &[256, 256]);
        m.queue_reads(CONFIG, &[u32::from_le_bytes([0x52, 0x54, 0, 0x12])]);
        m.queue_reads(CONFIG + 4, &[u32::from_le_bytes([0x34, 0x56, 0, 0])]);
        m
    }

    fn device() -> Fake {
        let ring = Box::leak(Box::new(Mem([0; 4096])));
        let mut bufs = std::vec::Vec::new();
        let pages: [Page; QUEUE_SIZE] = core::array::from_fn(|i| {
            let m = Box::leak(Box::new(Mem([0; 4096])));
            bufs.push(m.0.as_mut_ptr());
            Page {
                va: m.0.as_mut_ptr(),
                pa: PA + (i as u64 + 1) * 4096,
            }
        });
        let r = Page {
            va: ring.0.as_mut_ptr(),
            pa: PA,
        };
        let dev = unsafe { VirtioNet::new(mmio(F_MAC | 0xff00_0000), r, pages) }.unwrap();
        Fake { dev, ring, bufs }
    }

    fn u16_at(m: &Mem, off: usize) -> u16 {
        u16::from_le_bytes(m.0[off..off + 2].try_into().unwrap())
    }
    fn u64_at(m: &Mem, off: usize) -> u64 {
        u64::from_le_bytes(m.0[off..off + 8].try_into().unwrap())
    }

    /// デバイスのふり:受信のバッファ `id` に `frame` を書いて used リングに返す
    fn deliver(f: &mut Fake, id: usize, frame: &[u8]) {
        let used = queue_base(RX) + USED;
        let idx = u16_at(f.ring, used + 2);
        unsafe {
            core::ptr::write_bytes(f.bufs[id], 0, HDR_LEN);
            core::ptr::copy_nonoverlapping(frame.as_ptr(), f.bufs[id].add(HDR_LEN), frame.len());
        }
        let slot = idx as usize % QUEUE_SIZE;
        let at = used + 4 + slot * 8;
        f.ring.0[at..at + 4].copy_from_slice(&(id as u32).to_le_bytes());
        f.ring.0[at + 4..at + 8].copy_from_slice(&((HDR_LEN + frame.len()) as u32).to_le_bytes());
        f.ring.0[used + 2..used + 4].copy_from_slice(&idx.wrapping_add(1).to_le_bytes());
    }

    #[test]
    fn init_sets_up_both_queues_and_reads_the_mac() {
        let f = device();
        let w = f.dev.regs().writes();
        assert_eq!(w[0], (STATUS, 0), "reset first");
        // 受け入れる機能:MAC と VERSION_1 だけ(デバイスが出した他のビットは受けない)
        assert!(w.contains(&(DRIVER_FEATURES, F_MAC)));
        assert!(w.contains(&(DRIVER_FEATURES_SEL, 1)) && w.contains(&(DRIVER_FEATURES, 1)));
        for q in [RX, TX] {
            assert!(w.contains(&(QUEUE_SEL, q)));
            assert!(w.contains(&(QUEUE_DESC_LOW, (PA as usize + queue_base(q)) as u32)));
            assert!(w.contains(&(
                QUEUE_DEVICE_LOW,
                (PA as usize + queue_base(q) + USED) as u32
            )));
        }
        assert!(w.contains(&(
            STATUS,
            STATUS_ACK | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK
        )));
        assert_eq!(f.dev.mac(), [0x52, 0x54, 0, 0x12, 0x34, 0x56]);
        assert_eq!(f.dev.link_type(), LinkType::Ethernet);
    }

    #[test]
    fn all_receive_buffers_are_offered_to_the_device() {
        let f = device();
        // 受信の記述子 i はバッファのページ i の先頭、デバイスが書く(WRITE)
        for i in 0..QUEUE_SIZE {
            assert_eq!(
                u64_at(f.ring, queue_base(RX) + i * 16),
                PA + (i as u64 + 1) * 4096
            );
            assert_eq!(u16_at(f.ring, queue_base(RX) + i * 16 + 12), DESC_WRITE);
        }
        assert_eq!(
            u16_at(f.ring, queue_base(RX) + AVAIL + 2),
            QUEUE_SIZE as u16
        );
        assert_eq!(f.dev.regs().writes().last(), Some(&(QUEUE_NOTIFY, RX)));
    }

    #[test]
    fn received_frames_lose_the_header_and_the_buffer_is_offered_again() {
        let mut f = device();
        let mut out = [0u8; 1514];
        assert_eq!(f.dev.recv(&mut out), Ok(None));
        deliver(&mut f, 3, b"an ethernet frame");
        assert_eq!(f.dev.recv(&mut out), Ok(Some(17)));
        assert_eq!(&out[..17], b"an ethernet frame");
        // バッファ 3 がまた avail リングに載った
        let avail = queue_base(RX) + AVAIL;
        assert_eq!(u16_at(f.ring, avail + 2), QUEUE_SIZE as u16 + 1);
        assert_eq!(u16_at(f.ring, avail + 4), 3);
        assert_eq!(f.dev.recv(&mut out), Ok(None));
    }

    #[test]
    fn sent_frames_get_a_zero_header_and_are_announced() {
        let mut f = device();
        f.dev.send(b"outgoing").unwrap();
        let d = queue_base(TX);
        // 記述子 0 → バッファのページ 0 の後ろ半分、長さはヘッダ込み
        assert_eq!(u64_at(f.ring, d), PA + 4096 + BUF_LEN as u64);
        assert_eq!(u16_at(f.ring, d + 8), (HDR_LEN + 8) as u16);
        let tx = unsafe { core::slice::from_raw_parts(f.bufs[0].add(BUF_LEN), HDR_LEN + 8) };
        assert_eq!(&tx[..HDR_LEN], &[0; HDR_LEN]);
        assert_eq!(&tx[HDR_LEN..], b"outgoing");
        assert_eq!(u16_at(f.ring, d + AVAIL + 2), 1);
        assert_eq!(f.dev.regs().writes().last(), Some(&(QUEUE_NOTIFY, TX)));
    }

    #[test]
    fn sending_stops_when_every_buffer_is_in_flight() {
        let mut f = device();
        for _ in 0..QUEUE_SIZE {
            f.dev.send(b"x").unwrap();
        }
        assert_eq!(f.dev.send(b"x"), Err(NetError::Busy));
        // デバイスが 1 つ送り終えた(used の idx を 1 にする)
        let used = queue_base(TX) + USED;
        f.ring.0[used + 2] = 1;
        assert_eq!(f.dev.send(b"x"), Ok(()));
        assert_eq!(f.dev.send(&[0; BUF_LEN]), Err(NetError::TooLarge));
    }

    #[test]
    fn an_interrupt_is_acknowledged_with_the_reasons_it_had() {
        let mut m = FakeMmio::new();
        // 理由:1 = 使い終わったバッファがある(受信した・送り終わった)
        m.queue_reads(INTERRUPT_STATUS, &[1]);
        assert_eq!(ack_interrupt(&mut m), 1);
        assert_eq!(m.writes(), [(INTERRUPT_ACK, 1)]);
    }

    #[test]
    fn not_a_network_device_is_rejected() {
        let mut m = FakeMmio::new();
        m.queue_reads(MAGIC, &[0x7472_6976]);
        m.queue_reads(VERSION, &[2]);
        m.queue_reads(DEVICE_ID, &[2]); // ブロック
        let ring = Box::leak(Box::new(Mem([0; 4096])));
        let p = Page {
            va: ring.0.as_mut_ptr(),
            pa: PA,
        };
        assert!(unsafe { VirtioNet::new(m, p, [p; QUEUE_SIZE]) }.is_none());
    }
}
