//! AArch64 固有の処理。
//!
//! アセンブリやシステムレジスタを使う部分は `target_os = "none"`(カーネルのビルド)のときだけ
//! コンパイルする。ESR の解読のような純粋な部分はホストでも `cargo test` できる。
#![no_std]

#[cfg(test)]
extern crate std;

pub mod esr;
pub mod trap;

#[cfg(target_os = "none")]
pub mod boot;
#[cfg(target_os = "none")]
pub mod cpu;
#[cfg(target_os = "none")]
pub mod exception;
#[cfg(target_os = "none")]
pub mod semihosting;
