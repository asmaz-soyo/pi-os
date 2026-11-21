//! ループバック:送ったフレームが、そのまま自分に届くデバイス(`lo`、127.0.0.1)。

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use hal::net::{LinkType, NetDev, NetError};

/// 1 フレームの最大(Linux の lo と同じ 64KiB)
const MTU: usize = 65535;

#[derive(Default)]
pub struct Loopback {
    queue: VecDeque<Vec<u8>>,
}

impl Loopback {
    pub fn new() -> Self {
        Self::default()
    }
}

impl NetDev for Loopback {
    fn link_type(&self) -> LinkType {
        LinkType::Ip
    }

    fn mtu(&self) -> usize {
        MTU
    }

    fn send(&mut self, frame: &[u8]) -> Result<(), NetError> {
        if frame.len() > MTU {
            return Err(NetError::TooLarge);
        }
        self.queue.push_back(frame.to_vec());
        Ok(())
    }

    fn recv(&mut self, buf: &mut [u8]) -> Result<Option<usize>, NetError> {
        let Some(frame) = self.queue.pop_front() else {
            return Ok(None);
        };
        let n = frame.len().min(buf.len());
        buf[..n].copy_from_slice(&frame[..n]);
        Ok(Some(n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_come_back_in_order() {
        let mut lo = Loopback::new();
        let mut buf = [0u8; 16];
        assert_eq!(lo.recv(&mut buf), Ok(None));
        lo.send(b"one").unwrap();
        lo.send(b"two").unwrap();
        assert_eq!(lo.recv(&mut buf), Ok(Some(3)));
        assert_eq!(&buf[..3], b"one");
        assert_eq!(lo.recv(&mut buf), Ok(Some(3)));
        assert_eq!(&buf[..3], b"two");
        assert_eq!(lo.link_type(), LinkType::Ip);
    }
}
