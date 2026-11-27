//! パケットの記録(pcap)。インターフェースが送ったフレームと受け取ったフレームを取っておき、
//! pcap 形式(Wireshark や tcpdump で開ける)で取り出す。

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use hal::net::LinkType;

/// pcap のファイルの頭:マイクロ秒の時刻を使う形
const MAGIC: u32 = 0xa1b2_c3d4;
/// 1 つのパケットから取る最大の長さ
const SNAPLEN: u32 = 65535;
/// リンクの種類(tcpdump の LINKTYPE_*)
const LINKTYPE_ETHERNET: u32 = 1;
const LINKTYPE_RAW: u32 = 101;

/// 1 つのインターフェースの記録。中身の合計が `limit` バイトを超えたら、古いものから捨てる(リング)
pub struct Capture {
    link: LinkType,
    limit: usize,
    frames: VecDeque<(u64, Vec<u8>)>,
    bytes: usize,
    dropped: u64,
}

impl Capture {
    pub fn new(link: LinkType, limit: usize) -> Self {
        Self {
            link,
            limit,
            frames: VecDeque::new(),
            bytes: 0,
            dropped: 0,
        }
    }

    /// フレームを 1 つ記録する(`us` は起動してからのマイクロ秒)
    pub fn record(&mut self, us: u64, frame: &[u8]) {
        let frame = &frame[..frame.len().min(SNAPLEN as usize)];
        self.bytes += frame.len();
        self.frames.push_back((us, frame.to_vec()));
        while self.bytes > self.limit {
            let Some((_, old)) = self.frames.pop_front() else {
                break;
            };
            self.bytes -= old.len();
            self.dropped += 1;
        }
    }

    /// 限度を超えて捨てたフレームの数
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// 今までの記録を pcap のファイルの形にする
    pub fn pcap(&self) -> Vec<u8> {
        let link = match self.link {
            LinkType::Ethernet => LINKTYPE_ETHERNET,
            LinkType::Ip => LINKTYPE_RAW,
        };
        let mut out = Vec::with_capacity(24 + self.bytes + 16 * self.frames.len());
        // 頭(24 バイト):目印、版 2.4、時差 0、精度 0、切り詰めの長さ、リンクの種類
        for w in [MAGIC, 0x0004_0002, 0, 0, SNAPLEN, link] {
            out.extend_from_slice(&w.to_le_bytes());
        }
        // 1 つずつ(16 バイトの頭 + 中身):秒、マイクロ秒、取った長さ、元の長さ
        for (us, frame) in &self.frames {
            let len = frame.len() as u32;
            for w in [(us / 1_000_000) as u32, (us % 1_000_000) as u32, len, len] {
                out.extend_from_slice(&w.to_le_bytes());
            }
            out.extend_from_slice(frame);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_at(b: &[u8], i: usize) -> u32 {
        u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
    }

    #[test]
    fn the_file_starts_with_the_pcap_header_for_the_link_type() {
        let eth = Capture::new(LinkType::Ethernet, 1024).pcap();
        assert_eq!(eth.len(), 24);
        // マイクロ秒の pcap、版 2.4、切り詰めの長さ、リンクの種類(1 = Ethernet)
        assert_eq!(u32_at(&eth, 0), 0xa1b2_c3d4);
        assert_eq!(&eth[4..8], &[2, 0, 4, 0]);
        assert_eq!(u32_at(&eth, 16), 65535);
        assert_eq!(u32_at(&eth, 20), 1);
        // ループバックは IP のパケットそのもの(101 = LINKTYPE_RAW)
        assert_eq!(u32_at(&Capture::new(LinkType::Ip, 1024).pcap(), 20), 101);
    }

    #[test]
    fn each_frame_becomes_a_record_with_its_time() {
        let mut c = Capture::new(LinkType::Ethernet, 1024);
        c.record(3_000_250, b"abc");
        c.record(4_000_000, b"de");
        let p = c.pcap();
        // 秒、マイクロ秒、取った長さ、元の長さ、中身
        assert_eq!((u32_at(&p, 24), u32_at(&p, 28)), (3, 250));
        assert_eq!((u32_at(&p, 32), u32_at(&p, 36)), (3, 3));
        assert_eq!(&p[40..43], b"abc");
        assert_eq!((u32_at(&p, 43), u32_at(&p, 47)), (4, 0));
        assert_eq!(&p[59..61], b"de");
        assert_eq!(p.len(), 61);
    }

    #[test]
    fn old_frames_are_dropped_when_the_limit_is_reached() {
        // 限度は中身の合計のバイト数。超えたら古いものから捨てる
        let mut c = Capture::new(LinkType::Ethernet, 10);
        c.record(1, b"11111");
        c.record(2, b"22222");
        c.record(3, b"33333");
        let p = c.pcap();
        assert_eq!(p.len(), 24 + 2 * (16 + 5));
        assert_eq!(&p[40..45], b"22222");
        assert_eq!(c.dropped(), 1);
    }
}
