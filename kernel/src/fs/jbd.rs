//! ジャーナル(JBD2:Linux の ext3 / ext4 と同じ形式)。
//!
//! ファイルシステムを変える操作は、まず変えるブロックの新しい中身をジャーナル(ext3 では inode 8 の
//! 中の環状の領域)に書き、最後に「コミット」のブロックを書く。そのあとで本来の場所に書く。
//! 本来の場所に書いている途中で電源が切れても、次のマウントでジャーナルから書き直せば(リプレイ)
//! 食い違いが残らない。コミットまで書けていないトランザクションは、なかったことにする。
//!
//! ```text
//! ジャーナルのブロック 0:ジャーナルのスーパーブロック(大きさ、次に読む位置 s_start、次の番号 s_sequence)
//! トランザクション:記述ブロック(どのブロックの中身が続くか)→ 中身 … → コミットブロック
//!                 取り消しブロック(この番号のブロックは、前のトランザクションの中身を書き戻すな)
//! ```
//!
//! 数値はすべてビッグエンディアン。

use crate::Errno;
use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;

pub const MAGIC: u32 = 0xC03B_3998;
const DESCRIPTOR: u32 = 1;
const COMMIT: u32 = 2;
const SUPERBLOCK_V1: u32 = 3;
const SUPERBLOCK_V2: u32 = 4;
const REVOKE: u32 = 5;

/// 記述ブロックのタグの印
const TAG_ESCAPE: u16 = 1;
const TAG_SAME_UUID: u16 = 2;
const TAG_LAST: u16 = 8;

/// 扱える機能:取り消しブロック(REVOKE = 0x1)だけ。64 ビットのブロック番号(0x2)や
/// チェックサム(0x8、0x10)を求めるジャーナルは扱わない
const INCOMPAT_REVOKE: u32 = 0x1;
const INCOMPAT_SUPPORTED: u32 = INCOMPAT_REVOKE;

/// ジャーナルとファイルシステムの読み書き
pub trait JournalIo {
    fn block_size(&self) -> usize;
    /// ジャーナルの `n` 番目のブロック
    fn read_log(&self, n: u32, buf: &mut [u8]) -> Result<(), Errno>;
    fn write_log(&self, n: u32, buf: &[u8]) -> Result<(), Errno>;
    /// ファイルシステムの本来の場所(ブロック番号)に書く
    fn write_fs(&self, block: u32, buf: &[u8]) -> Result<(), Errno>;
}

pub fn be32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes(b[off..off + 4].try_into().unwrap())
}

fn be16(b: &[u8], off: usize) -> u16 {
    u16::from_be_bytes([b[off], b[off + 1]])
}

pub fn put_be32(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_be_bytes());
}

/// ジャーナルのスーパーブロック
#[derive(Debug, Clone, Copy)]
pub struct Super {
    /// ジャーナルの大きさ(ブロック)
    pub maxlen: u32,
    /// ログの領域の最初のブロック(ふつう 1)
    pub first: u32,
    /// 次に書く(読む)トランザクションの番号
    pub sequence: u32,
    /// ログの始まり。0 ならジャーナルは空
    pub start: u32,
}

pub fn read_super(io: &impl JournalIo) -> Result<Super, Errno> {
    let mut b = vec![0u8; io.block_size()];
    io.read_log(0, &mut b)?;
    let kind = be32(&b, 4);
    if be32(&b, 0) != MAGIC || (kind != SUPERBLOCK_V1 && kind != SUPERBLOCK_V2) {
        return Err(Errno::EINVAL);
    }
    if be32(&b, 12) as usize != io.block_size() {
        return Err(Errno::EINVAL);
    }
    if kind == SUPERBLOCK_V2 && be32(&b, 40) & !INCOMPAT_SUPPORTED != 0 {
        return Err(Errno::EINVAL);
    }
    Ok(Super {
        maxlen: be32(&b, 16),
        first: be32(&b, 20),
        sequence: be32(&b, 24),
        start: be32(&b, 28),
    })
}

/// スーパーブロックの、次の番号(s_sequence)とログの始まり(s_start)を書く
pub fn write_super(io: &impl JournalIo, sequence: u32, start: u32) -> Result<(), Errno> {
    let mut b = vec![0u8; io.block_size()];
    io.read_log(0, &mut b)?;
    put_be32(&mut b, 24, sequence);
    put_be32(&mut b, 28, start);
    io.write_log(0, &b)
}

impl Super {
    /// ログの中で、`n` の次のブロック(終わりまで来たら最初に戻る:環状)
    pub fn next(&self, n: u32) -> u32 {
        if n + 1 >= self.maxlen {
            self.first
        } else {
            n + 1
        }
    }
}

/// 記述ブロックのタグ:(本来の場所のブロック番号, 先頭の 4 バイトが逃がしてあるか)
fn tags(block: &[u8]) -> Vec<(u32, bool)> {
    let mut out = Vec::new();
    let mut at = 12;
    while at + 8 <= block.len() {
        let target = be32(block, at);
        let flags = be16(block, at + 6);
        out.push((target, flags & TAG_ESCAPE != 0));
        at += 8;
        if flags & TAG_SAME_UUID == 0 {
            at += 16; // UUID が続く
        }
        if flags & TAG_LAST != 0 {
            break;
        }
    }
    out
}

/// リプレイした結果
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Recovered {
    /// リプレイしたトランザクションの数
    pub transactions: u32,
    /// 本来の場所に書いたブロックの数
    pub blocks: usize,
}

/// ジャーナルに残っているコミット済みのトランザクションを、本来の場所に書く。
/// 終わったらジャーナルを空にする(s_start = 0)。
pub fn recover(io: &impl JournalIo) -> Result<Recovered, Errno> {
    let sb = read_super(io)?;
    if sb.start == 0 {
        return Ok(Recovered::default());
    }
    let bs = io.block_size();
    let mut buf = vec![0u8; bs];

    // 1 回目:コミットまで書けているトランザクションの範囲を探す
    let mut pos = sb.start;
    let mut seq = sb.sequence;
    let mut committed_end = sb.sequence; // この番号の手前まで
    loop {
        io.read_log(pos, &mut buf)?;
        if be32(&buf, 0) != MAGIC || be32(&buf, 8) != seq {
            break;
        }
        match be32(&buf, 4) {
            DESCRIPTOR => {
                for _ in tags(&buf) {
                    pos = sb.next(pos);
                }
            }
            COMMIT => {
                seq = seq.wrapping_add(1);
                committed_end = seq;
            }
            REVOKE => {}
            _ => break,
        }
        pos = sb.next(pos);
    }

    // 2 回目:取り消し(ブロック → 取り消したトランザクションの番号の最大)
    let mut revoked: BTreeMap<u32, u32> = BTreeMap::new();
    walk(io, &sb, committed_end, &mut buf, |kind, seq, block, _| {
        if kind == REVOKE {
            let used = (be32(block, 12) as usize).min(bs);
            let mut at = 16;
            while at + 4 <= used {
                let target = be32(block, at);
                let e = revoked.entry(target).or_insert(seq);
                *e = (*e).max(seq);
                at += 4;
            }
        }
        Ok(())
    })?;

    // 3 回目:中身を本来の場所に書く(後で取り消されたものは書かない)
    let mut result = Recovered {
        transactions: committed_end.wrapping_sub(sb.sequence),
        blocks: 0,
    };
    let mut data = vec![0u8; bs];
    walk(
        io,
        &sb,
        committed_end,
        &mut buf,
        |kind, seq, block, mut data_pos| {
            if kind != DESCRIPTOR {
                return Ok(());
            }
            for (target, escaped) in tags(block) {
                data_pos = sb.next(data_pos);
                if revoked.get(&target).is_some_and(|&r| r >= seq) {
                    continue;
                }
                io.read_log(data_pos, &mut data)?;
                if escaped {
                    // 中身がたまたまジャーナルの印で始まっていたので、書くときに 0 にしてあった
                    put_be32(&mut data, 0, MAGIC);
                }
                io.write_fs(target, &data)?;
                result.blocks += 1;
            }
            Ok(())
        },
    )?;

    write_super(io, committed_end, 0)?;
    Ok(result)
}

/// コミット済みの範囲のブロックを順に見る:f(種類, トランザクションの番号, ブロック, その位置)
fn walk(
    io: &impl JournalIo,
    sb: &Super,
    end: u32,
    buf: &mut [u8],
    mut f: impl FnMut(u32, u32, &[u8], u32) -> Result<(), Errno>,
) -> Result<(), Errno> {
    let mut pos = sb.start;
    let mut seq = sb.sequence;
    while seq != end {
        io.read_log(pos, buf)?;
        if be32(buf, 0) != MAGIC || be32(buf, 8) != seq {
            break;
        }
        let kind = be32(buf, 4);
        f(kind, seq, buf, pos)?;
        match kind {
            DESCRIPTOR => {
                for _ in tags(buf) {
                    pos = sb.next(pos);
                }
            }
            COMMIT => seq = seq.wrapping_add(1),
            _ => {}
        }
        pos = sb.next(pos);
    }
    Ok(())
}
