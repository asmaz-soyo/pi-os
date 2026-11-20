//! アドレス空間(仮想アドレス → 物理アドレスの対応)のポート。

/// マッピングの属性。読み出しは常に許可する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MapFlags {
    pub write: bool,
    pub exec: bool,
    /// ユーザー(EL0)から触れる
    pub user: bool,
    /// デバイスのレジスタ(キャッシュしない、順序を守る)
    pub device: bool,
}

impl MapFlags {
    /// カーネルのデータ(読み書き)
    pub const KERNEL_RW: Self = Self {
        write: true,
        exec: false,
        user: false,
        device: false,
    };
    /// カーネルのコード(読み・実行)
    pub const KERNEL_RX: Self = Self {
        write: false,
        exec: true,
        user: false,
        device: false,
    };
    /// カーネルのコードとデータ(読み書き・実行。起動直後の大まかなマッピング用)
    pub const KERNEL_RWX: Self = Self {
        write: true,
        exec: true,
        user: false,
        device: false,
    };
    /// デバイスのレジスタ
    pub const DEVICE: Self = Self {
        write: true,
        exec: false,
        user: false,
        device: true,
    };
    /// ユーザーのデータ
    pub const USER_RW: Self = Self {
        write: true,
        exec: false,
        user: true,
        device: false,
    };
    /// ユーザーのコード
    pub const USER_RX: Self = Self {
        write: false,
        exec: true,
        user: true,
        device: false,
    };
    /// ユーザーの読み出し専用データ
    pub const USER_R: Self = Self {
        write: false,
        exec: false,
        user: true,
        device: false,
    };
}

use crate::addr::{PhysAddr, VirtAddr};

/// マッピングの失敗。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    AlreadyMapped,
    Misaligned,
    OutOfMemory,
}

/// プロセスのアドレス空間(ユーザー空間のページテーブル)。
pub trait AddressSpace: Send {
    /// 4KiB ページを1枚マップする。
    fn map_page(&mut self, va: VirtAddr, pa: PhysAddr, flags: MapFlags) -> Result<(), MapError>;
    /// マッピングを外し、指していたフレームを返す(TLB の無効化も行う)。
    fn unmap_page(&mut self, va: VirtAddr) -> Option<PhysAddr>;
    /// 変換する。マップされていなければ `None`。
    fn translate(&mut self, va: VirtAddr) -> Option<(PhysAddr, MapFlags)>;
    /// このアドレス空間を CPU に設定する(TTBR0 を切り替える)。
    fn activate(&mut self);
}

/// 物理フレームの確保と、その中身への読み書き。
pub trait PhysMemory: Sync {
    /// 0 で埋めたフレームを1枚取る。
    fn alloc_frame(&self) -> Option<PhysAddr>;
    /// フレームを返す。
    fn release_frame(&self, pa: PhysAddr);
    /// フレームの中身を指すポインタ(カーネルから読み書きするため)。
    ///
    /// # Safety
    /// `pa` は `alloc_frame` で取ったフレームであること。ポインタは 4KiB の範囲だけ有効。
    unsafe fn frame_ptr(&self, pa: PhysAddr) -> *mut u8;
}
