#![no_std]

#[cfg(any(test, feature = "mock"))]
extern crate std;

pub mod console;

#[cfg(any(test, feature = "mock"))]
pub mod mock;
