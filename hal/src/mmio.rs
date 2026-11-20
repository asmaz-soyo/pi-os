//! メモリマップド I/O(MMIO)のレジスタ読み書き。
//!
//! ドライバはこのトレイト越しにレジスタを触る。実機では [`MmioRegion`] が volatile で
//! 物理アドレスを読み書きし、テストでは `mock::FakeMmio` が書き込みを記録する。
//! レジスタの読み出しには副作用がある(FIFO から1文字取り出すなど)ので、`read` も `&mut self`。

/// ベースアドレスからのオフセットで 32 ビットのレジスタを読み書きする。
pub trait Mmio {
    fn read(&mut self, offset: usize) -> u32;
    fn write(&mut self, offset: usize, value: u32);
}

/// 実際の MMIO 領域。
#[derive(Debug)]
pub struct MmioRegion {
    base: usize,
}

impl MmioRegion {
    /// # Safety
    /// `base` からの領域が、このデバイスのレジスタとしてマップされていること。
    /// 同じ領域を指す `MmioRegion` を複数作らないこと。
    pub const unsafe fn new(base: usize) -> Self {
        Self { base }
    }

    pub fn base(&self) -> usize {
        self.base
    }
}

impl Mmio for MmioRegion {
    fn read(&mut self, offset: usize) -> u32 {
        // SAFETY: new の契約により、base + offset はこのデバイスのレジスタ
        unsafe { core::ptr::read_volatile((self.base + offset) as *const u32) }
    }

    fn write(&mut self, offset: usize, value: u32) {
        // SAFETY: new の契約により、base + offset はこのデバイスのレジスタ
        unsafe { core::ptr::write_volatile((self.base + offset) as *mut u32, value) }
    }
}
