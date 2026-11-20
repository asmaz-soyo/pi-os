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
