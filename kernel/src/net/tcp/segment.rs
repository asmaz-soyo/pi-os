//! TCP のセグメントを、バイト列から読む・バイト列に書く(RFC 9293 の 3.1)。
//!
//! ```text
//!  0               1               2               3
//! +---------------+---------------+---------------+---------------+
//! |          送り元ポート          |          宛先ポート            |
//! +---------------+---------------+---------------+---------------+
//! |                        シーケンス番号                          |
//! +---------------------------------------------------------------+
//! |                         確認応答番号                           |
//! +-------+-------+---------------+-------------------------------+
//! |オフセット| 予約 |    フラグ      |           ウィンドウ            |
//! +-------+-------+---------------+-------------------------------+
//! |         チェックサム            |         緊急ポインタ            |
//! +---------------------------------------------------------------+
//! |                 オプション(4 バイト単位で 0〜40)                |
//! +---------------------------------------------------------------+
//! |                            データ                              |
//! ```

use crate::Errno;
use alloc::vec::Vec;
use core::net::Ipv4Addr;

/// フラグ(ヘッダの 13 バイト目のビット)
pub const FIN: u8 = 0x01;
pub const SYN: u8 = 0x02;
pub const RST: u8 = 0x04;
pub const PSH: u8 = 0x08;
pub const ACK: u8 = 0x10;

/// オプションのないヘッダの長さ
pub const HEADER_LEN: usize = 20;

/// 1 つのセグメント。`payload` は読んだバイト列を借りる
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment<'a> {
    pub src_port: u16,
    pub dst_port: u16,
    pub seq: u32,
    /// `flags` に ACK があるときだけ意味がある
    pub ack: u32,
    pub flags: u8,
    pub window: u16,
    /// MSS オプション(SYN にだけ付ける)
    pub mss: Option<u16>,
    pub payload: &'a [u8],
}

/// オプションの種類
const OPT_END: u8 = 0;
const OPT_NOP: u8 = 1;
const OPT_MSS: u8 = 2;

impl Segment<'_> {
    pub fn has(&self, flag: u8) -> bool {
        self.flags & flag == flag
    }

    /// シーケンス番号をいくつ使うか(RFC の SEG.LEN。データの長さ + SYN と FIN は 1 ずつ)
    pub fn seq_len(&self) -> u32 {
        self.payload.len() as u32 + self.has(SYN) as u32 + self.has(FIN) as u32
    }

    /// バイト列にする(チェックサムも入れる)
    pub fn emit(&self, src: Ipv4Addr, dst: Ipv4Addr) -> Vec<u8> {
        let header_len = HEADER_LEN + if self.mss.is_some() { 4 } else { 0 };
        let mut b = Vec::with_capacity(header_len + self.payload.len());
        b.extend_from_slice(&self.src_port.to_be_bytes());
        b.extend_from_slice(&self.dst_port.to_be_bytes());
        b.extend_from_slice(&self.seq.to_be_bytes());
        let ack = if self.has(ACK) { self.ack } else { 0 };
        b.extend_from_slice(&ack.to_be_bytes());
        b.push(((header_len / 4) as u8) << 4); // データオフセット(4 バイト単位)
        b.push(self.flags);
        b.extend_from_slice(&self.window.to_be_bytes());
        b.extend_from_slice(&[0, 0, 0, 0]); // チェックサム(あとで入れる)と緊急ポインタ
        if let Some(mss) = self.mss {
            b.extend_from_slice(&[OPT_MSS, 4]);
            b.extend_from_slice(&mss.to_be_bytes());
        }
        b.extend_from_slice(self.payload);
        let sum = checksum(src, dst, &b);
        b[16..18].copy_from_slice(&sum.to_be_bytes());
        b
    }
}

impl<'a> Segment<'a> {
    /// バイト列から読む。短すぎる・オフセットがおかしい・チェックサムが合わないものは EINVAL
    pub fn parse(src: Ipv4Addr, dst: Ipv4Addr, bytes: &'a [u8]) -> Result<Self, Errno> {
        if bytes.len() < HEADER_LEN {
            return Err(Errno::EINVAL);
        }
        let header_len = (bytes[12] >> 4) as usize * 4;
        if header_len < HEADER_LEN || header_len > bytes.len() {
            return Err(Errno::EINVAL);
        }
        // 正しいチェックサムが入っていれば、全体を足すと 0xffff になり、反転して 0
        if checksum(src, dst, bytes) != 0 {
            return Err(Errno::EINVAL);
        }
        let u16_at = |i: usize| u16::from_be_bytes([bytes[i], bytes[i + 1]]);
        let u32_at =
            |i: usize| u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
        Ok(Segment {
            src_port: u16_at(0),
            dst_port: u16_at(2),
            seq: u32_at(4),
            ack: u32_at(8),
            flags: bytes[13] & (FIN | SYN | RST | PSH | ACK),
            window: u16_at(14),
            mss: mss_option(&bytes[HEADER_LEN..header_len])?,
            payload: &bytes[header_len..],
        })
    }
}

/// オプションの並びから MSS を探す。種類と長さの組を順に読み、知らないものは長さの分だけ飛ばす
fn mss_option(mut opts: &[u8]) -> Result<Option<u16>, Errno> {
    let mut mss = None;
    while let [kind, rest @ ..] = opts {
        match *kind {
            OPT_END => break,
            OPT_NOP => opts = rest,
            _ => {
                let len = *rest.first().ok_or(Errno::EINVAL)? as usize;
                if len < 2 || len > opts.len() {
                    return Err(Errno::EINVAL);
                }
                if *kind == OPT_MSS && len == 4 {
                    mss = Some(u16::from_be_bytes([opts[2], opts[3]]));
                }
                opts = &opts[len..];
            }
        }
    }
    Ok(mss)
}

/// TCP のチェックサム:疑似ヘッダ(送り元・宛先のアドレス、0、プロトコル番号 6、長さ)と
/// セグメント全体を、16 ビットずつ 1 の補数で足して反転したもの
pub fn checksum(src: Ipv4Addr, dst: Ipv4Addr, segment: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut add = |bytes: &[u8]| {
        for pair in bytes.chunks(2) {
            // 奇数の長さなら、最後のバイトの後ろに 0 を足して 16 ビットにする
            sum += u16::from_be_bytes([pair[0], *pair.get(1).unwrap_or(&0)]) as u32;
        }
    };
    add(&src.octets());
    add(&dst.octets());
    add(&[0, 6]);
    add(&(segment.len() as u16).to_be_bytes());
    add(segment);
    // あふれた上の桁を下に足し戻す(1 の補数の足し算)
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use smoltcp::phy::ChecksumCapabilities;
    use smoltcp::wire::{IpAddress, TcpControl, TcpPacket, TcpRepr, TcpSeqNumber};

    const A: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 15);
    const B: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);

    fn syn() -> Segment<'static> {
        Segment {
            src_port: 49152,
            dst_port: 80,
            seq: 0xdead_beef,
            ack: 0,
            flags: SYN,
            window: 4096,
            mss: Some(1460),
            payload: &[],
        }
    }

    #[test]
    fn emitted_segments_are_read_the_same_way_by_smoltcp() {
        let data = Segment {
            src_port: 80,
            dst_port: 49152,
            seq: 1000,
            ack: 0xdead_bef0,
            flags: ACK | PSH,
            window: 2048,
            mss: None,
            payload: b"hello, tcp",
        };
        for seg in [syn(), data] {
            let bytes = seg.emit(A, B);
            let p = TcpPacket::new_checked(&bytes[..]).unwrap();
            assert!(p.verify_checksum(&IpAddress::Ipv4(A), &IpAddress::Ipv4(B)));
            assert_eq!(p.src_port(), seg.src_port);
            assert_eq!(p.dst_port(), seg.dst_port);
            assert_eq!(p.seq_number(), TcpSeqNumber(seg.seq as i32));
            assert_eq!(p.syn(), seg.has(SYN));
            assert_eq!(p.ack(), seg.has(ACK));
            assert_eq!(p.psh(), seg.has(PSH));
            if seg.has(ACK) {
                assert_eq!(p.ack_number(), TcpSeqNumber(seg.ack as i32));
            }
            assert_eq!(p.window_len(), seg.window);
            assert_eq!(p.payload(), seg.payload);
            let repr = TcpRepr::parse(
                &p,
                &IpAddress::Ipv4(A),
                &IpAddress::Ipv4(B),
                &ChecksumCapabilities::default(),
            )
            .unwrap();
            assert_eq!(repr.max_seg_size, seg.mss);
        }
    }

    #[test]
    fn segments_emitted_by_smoltcp_are_parsed() {
        let repr = TcpRepr {
            src_port: 80,
            dst_port: 49152,
            control: TcpControl::Syn,
            seq_number: TcpSeqNumber(7),
            ack_number: Some(TcpSeqNumber(0xdead_bef0u32 as i32)),
            window_len: 1024,
            window_scale: Some(7),
            max_seg_size: Some(536),
            sack_permitted: true,
            sack_ranges: [None; 3],
            timestamp: None,
            payload: &[],
        };
        let mut bytes = vec![0; repr.buffer_len()];
        repr.emit(
            &mut TcpPacket::new_unchecked(&mut bytes[..]),
            &IpAddress::Ipv4(B),
            &IpAddress::Ipv4(A),
            &ChecksumCapabilities::default(),
        );
        // 知らないオプション(ウィンドウスケール、SACK)は読み飛ばす
        let seg = Segment::parse(B, A, &bytes).unwrap();
        assert_eq!(
            seg,
            Segment {
                src_port: 80,
                dst_port: 49152,
                seq: 7,
                ack: 0xdead_bef0,
                flags: SYN | ACK,
                window: 1024,
                mss: Some(536),
                payload: &[],
            }
        );
    }

    #[test]
    fn broken_segments_are_rejected() {
        let good = Segment {
            payload: b"abc",
            ..syn()
        }
        .emit(A, B);
        assert!(Segment::parse(A, B, &good).is_ok());
        // データが 1 ビット化けた
        let mut flipped = good.clone();
        *flipped.last_mut().unwrap() ^= 1;
        assert_eq!(Segment::parse(A, B, &flipped), Err(Errno::EINVAL));
        // 宛先が違う(疑似ヘッダに入るので、チェックサムが合わない)
        assert_eq!(Segment::parse(A, A, &good), Err(Errno::EINVAL));
        // ヘッダより短い
        assert_eq!(Segment::parse(A, B, &good[..19]), Err(Errno::EINVAL));
        // データオフセットが 5 未満・全体より長い
        for offset in [4u8, 15] {
            let mut bad = good.clone();
            bad[12] = offset << 4;
            assert_eq!(Segment::parse(A, B, &bad), Err(Errno::EINVAL));
        }
    }

    #[test]
    fn checksum_of_an_odd_length_segment_pads_with_zero() {
        // 長さが奇数なら、最後のバイトの後ろに 0 があるものとして足す
        let seg = Segment {
            payload: b"x",
            ..syn()
        }
        .emit(A, B);
        assert_eq!(seg.len() % 2, 1);
        let p = TcpPacket::new_checked(&seg[..]).unwrap();
        assert!(p.verify_checksum(&IpAddress::Ipv4(A), &IpAddress::Ipv4(B)));
        // 自分のチェックサムを入れたまま計算し直すと 0 になる
        assert_eq!(checksum(A, B, &seg), 0);
    }

    #[test]
    fn syn_and_fin_each_use_one_sequence_number() {
        assert_eq!(syn().seq_len(), 1);
        let data = Segment {
            flags: ACK | FIN,
            mss: None,
            payload: b"bye",
            ..syn()
        };
        assert_eq!(data.seq_len(), 4);
        assert_eq!(Segment { flags: ACK, ..data }.seq_len(), 3);
    }
}
