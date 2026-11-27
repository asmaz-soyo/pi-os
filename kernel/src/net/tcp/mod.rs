//! 自作の TCP(ADR-0006 の差し替え先)。
//!
//! smoltcp の代わりに、ここで TCP を一から組み立てる。IP・ARP・DHCP・UDP は smoltcp のまま。
//!
//! - [`segment`]:セグメント(TCP のヘッダとデータ)とバイト列の行き来、チェックサム
//! - [`seq`]:32 ビットで一周するシーケンス番号の比べ方
//! - [`conn`]:1 本の接続の状態遷移(ハードウェアにも時計にも触らない)

pub mod conn;
pub mod segment;
pub mod seq;
