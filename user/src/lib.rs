//! initramfs:起動時にメモリに置くファイルの束(tar)。
//!
//! `build.rs` が `programs/` をビルドした `/bin/*` と、`rootfs/` の下のファイルをまとめたもの。
#![no_std]

pub static INITRAMFS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/initramfs.tar"));
