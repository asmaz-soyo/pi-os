//! メモリの上のディスク(テストと開発用)。

use alloc::vec;
use alloc::vec::Vec;
use hal::block::{BLOCK_SIZE, BlockDevice, BlockError};

pub struct RamDisk {
    data: Vec<u8>,
}

impl RamDisk {
    /// 同じ中身の別のディスク(電源断のあとの「そのときのディスク」を取り出すテスト用)
    pub fn clone_disk(&self) -> crate::block::SharedDisk {
        crate::block::shared(alloc::boxed::Box::new(Self {
            data: self.data.clone(),
        }))
    }

    /// `blocks` 個のブロックを 0 で埋めて作る
    pub fn new(blocks: usize) -> Self {
        Self {
            data: vec![0; blocks * BLOCK_SIZE],
        }
    }

    /// 中身を渡して作る(大きさは 512 バイトの倍数に切り上げる)
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut data = bytes.to_vec();
        data.resize(bytes.len().div_ceil(BLOCK_SIZE) * BLOCK_SIZE, 0);
        Self { data }
    }

    fn range(&self, lba: u64) -> Result<core::ops::Range<usize>, BlockError> {
        let start = (lba as usize)
            .checked_mul(BLOCK_SIZE)
            .ok_or(BlockError::OutOfRange)?;
        if start + BLOCK_SIZE > self.data.len() {
            return Err(BlockError::OutOfRange);
        }
        Ok(start..start + BLOCK_SIZE)
    }
}

impl BlockDevice for RamDisk {
    fn block_count(&self) -> u64 {
        (self.data.len() / BLOCK_SIZE) as u64
    }

    fn read_block(&mut self, lba: u64, buf: &mut [u8; BLOCK_SIZE]) -> Result<(), BlockError> {
        let r = self.range(lba)?;
        buf.copy_from_slice(&self.data[r]);
        Ok(())
    }

    fn write_block(&mut self, lba: u64, buf: &[u8; BLOCK_SIZE]) -> Result<(), BlockError> {
        let r = self.range(lba)?;
        self.data[r].copy_from_slice(buf);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hal::block::{BLOCK_SIZE, BlockDevice, BlockError};

    #[test]
    fn written_blocks_are_read_back() {
        let mut d = RamDisk::new(4);
        assert_eq!(d.block_count(), 4);
        let data = [7u8; BLOCK_SIZE];
        d.write_block(2, &data).unwrap();
        let mut buf = [0u8; BLOCK_SIZE];
        d.read_block(2, &mut buf).unwrap();
        assert_eq!(buf, data);
        d.read_block(1, &mut buf).unwrap();
        assert_eq!(buf, [0u8; BLOCK_SIZE]);
    }

    #[test]
    fn access_past_the_end_is_out_of_range() {
        let mut d = RamDisk::new(1);
        let mut buf = [0u8; BLOCK_SIZE];
        assert_eq!(d.read_block(1, &mut buf), Err(BlockError::OutOfRange));
    }
}
