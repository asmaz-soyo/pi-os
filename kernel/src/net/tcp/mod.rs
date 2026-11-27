//! 自作の TCP(ADR-0006 の差し替え先)。
//!
//! smoltcp の代わりに、ここで TCP を一から組み立てる。IP・ARP・DHCP・UDP は smoltcp のまま。
//!
//! - [`segment`]:セグメント(TCP のヘッダとデータ)とバイト列の行き来、チェックサム

pub mod segment;
