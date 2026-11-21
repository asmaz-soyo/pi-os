//! ネットワークデバイスのポート。
//!
//! ドライバ(virtio-net、実機の LAN など)はこの形でフレームを送り受けする。
//! 中身のプロトコル(IP、ARP…)はカーネルのネットワークスタックが扱い、ドライバは知らない。

/// リンク層の種類(フレームの先頭に何が付いているか)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkType {
    /// Ethernet(宛先 MAC・送り元 MAC・EtherType の 14 バイトのヘッダが付く)
    Ethernet,
    /// IP のパケットがそのまま(ループバック、トンネルなど)
    Ip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetError {
    /// 送る場所がいっぱい(あとでもう一度)
    Busy,
    /// フレームが大きすぎる
    TooLarge,
    /// デバイスが失敗を返した
    Io,
}

/// フレームを送り受けできるデバイス。
pub trait NetDev: Send {
    fn link_type(&self) -> LinkType;
    /// MAC アドレス(Ethernet のとき)
    fn mac(&self) -> [u8; 6] {
        [0; 6]
    }
    /// 1 フレームの中身(リンク層のヘッダを除く)の最大の大きさ
    fn mtu(&self) -> usize;
    /// フレームを 1 つ送る
    fn send(&mut self, frame: &[u8]) -> Result<(), NetError>;
    /// 届いたフレームを 1 つ `buf` に読む。何も届いていなければ `Ok(None)`
    fn recv(&mut self, buf: &mut [u8]) -> Result<Option<usize>, NetError>;
}
