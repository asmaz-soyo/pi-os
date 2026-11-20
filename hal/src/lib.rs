#![no_std]

#[cfg(any(test, feature = "mock"))]
extern crate std;

pub mod addr;
pub mod console;
pub mod irq;
pub mod mmio;
pub mod timer;

#[cfg(any(test, feature = "mock"))]
pub mod mock;
