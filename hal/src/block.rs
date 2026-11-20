//! ブロックデバイス(ディスク)のポート。

/// ブロック(セクタ)の大きさ
pub const BLOCK_SIZE: usize = 512;

/// 512 バイト単位で読み書きできるデバイス。
pub trait BlockDevice: Send {
    /// ブロックの数
    fn block_count(&self) -> u64;
    /// ブロック `lba` を `buf`(512 バイト)に読む
    fn read_block(&mut self, lba: u64, buf: &mut [u8; BLOCK_SIZE]) -> Result<(), BlockError>;
    /// `buf` をブロック `lba` に書く
    fn write_block(&mut self, lba: u64, buf: &[u8; BLOCK_SIZE]) -> Result<(), BlockError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockError {
    /// 範囲の外
    OutOfRange,
    /// デバイスが失敗を返した
    Io,
}
