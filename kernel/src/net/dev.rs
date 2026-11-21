//! ネットワークデバイスの登録表(`eth0`、`eth1`、…)。
//!
//! ドライバが見つけたデバイスを名前で登録しておく。IP の設定(DHCP など)をするときに取り出して、
//! ネットワークスタックにつなぐ。

use crate::sync::SpinLock;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use hal::net::NetDev;

struct Entry {
    name: String,
    mac: [u8; 6],
    /// まだスタックにつないでいなければ、ここにある
    dev: Option<Box<dyn NetDev>>,
}

static DEVICES: SpinLock<Vec<Entry>> = SpinLock::new(Vec::new());

/// デバイスを登録し、付けた名前(`eth0` から順に)を返す
pub fn register(dev: Box<dyn NetDev>) -> String {
    let mut devices = DEVICES.lock();
    let name = alloc::format!("eth{}", devices.len());
    devices.push(Entry {
        name: name.clone(),
        mac: dev.mac(),
        dev: Some(dev),
    });
    name
}

/// 登録したデバイスの名前
pub fn names() -> Vec<String> {
    DEVICES.lock().iter().map(|e| e.name.clone()).collect()
}

pub fn mac_of(name: &str) -> Option<[u8; 6]> {
    DEVICES
        .lock()
        .iter()
        .find(|e| e.name == name)
        .map(|e| e.mac)
}

/// デバイスを取り出す(スタックにつなぐため)。もう取り出してあれば None
pub fn take(name: &str) -> Option<Box<dyn NetDev>> {
    DEVICES
        .lock()
        .iter_mut()
        .find(|e| e.name == name)
        .and_then(|e| e.dev.take())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hal::net::{LinkType, NetError};

    struct Fake;

    impl NetDev for Fake {
        fn link_type(&self) -> LinkType {
            LinkType::Ethernet
        }
        fn mac(&self) -> [u8; 6] {
            [2, 0, 0, 0, 0, 9]
        }
        fn mtu(&self) -> usize {
            1500
        }
        fn send(&mut self, _: &[u8]) -> Result<(), NetError> {
            Ok(())
        }
        fn recv(&mut self, _: &mut [u8]) -> Result<Option<usize>, NetError> {
            Ok(None)
        }
    }

    #[test]
    fn devices_are_named_in_order_and_taken_once() {
        let name = register(Box::new(Fake));
        assert!(name.starts_with("eth"));
        assert!(names().contains(&name));
        assert_eq!(mac_of(&name), Some([2, 0, 0, 0, 0, 9]));
        assert!(take(&name).is_some());
        assert!(take(&name).is_none(), "already taken");
        // 取り出したあとも、名前と MAC は残る
        assert_eq!(mac_of(&name), Some([2, 0, 0, 0, 0, 9]));
    }
}
