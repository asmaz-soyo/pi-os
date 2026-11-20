//! AArch64 固有の処理。起動直後のアセンブリと、セミホスティング。
#![no_std]

pub mod boot;
pub mod cpu;
pub mod semihosting;
