//! 1 本の TCP 接続(TCB:Transmission Control Block。RFC 9293 の 3.3 と 3.10)。
//!
//! ハードウェアにも時計にも触らない。受け取ったセグメントを [`Conn::on_segment`] に渡し、
//! 送るものを [`Conn::dispatch`] で取り出す。だからホストのテストで、2 つの `Conn` を
//! 「線」でつないで確かめられる。

use super::segment::{ACK, FIN, PSH, RST, SYN, Segment};
use super::seq::Seq;
use crate::Errno;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::net::Ipv4Addr;

/// 接続の状態(RFC 9293 の 3.3.2)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Closed,
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    Closing,
    LastAck,
    TimeWait,
}

/// アドレスとポートの組
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Endpoint {
    pub addr: Ipv4Addr,
    pub port: u16,
}

/// こちらが受け取れるセグメントの大きさ(Ethernet の 1500 − IP 20 − TCP 20)
pub const OUR_MSS: u16 = 1460;
/// 相手が MSS を言わなかったときの大きさ(RFC 9293 の 3.7.1)
pub const DEFAULT_MSS: u16 = 536;
/// 再送の待ち時間:最初・下限・上限(ナノ秒。RFC 6298 は下限 1 秒だが、Linux と同じく 200ms)
pub const INITIAL_RTO: u64 = 1_000_000_000;
pub const MIN_RTO: u64 = 200_000_000;
pub const MAX_RTO: u64 = 60_000_000_000;
/// TIME-WAIT で待つ時間(2MSL。Linux と同じ 60 秒)
pub const TIME_WAIT: u64 = 60_000_000_000;
/// 時計の細かさ(RFC 6298 の G)
const GRANULARITY: u64 = 1_000_000;
/// 時間切れで送り直す回数の上限(SYN は 5 回、つながったあとは 8 回。Linux は 6 回と 15 回)
const SYN_RETRIES: u32 = 5;
const DATA_RETRIES: u32 = 8;

pub struct Conn {
    pub state: State,
    pub local: Endpoint,
    pub remote: Endpoint,
    /// 最初に使った番号(SYN の番号)
    iss: Seq,
    /// 相手がまだ確認していない、いちばん古い番号
    snd_una: Seq,
    /// 次に送る番号。再送はこれを snd_una に戻すだけでよい(送るものは全部ここから決める)
    snd_nxt: Seq,
    /// 相手のウィンドウ(snd_una から先に、いくつ送ってよいか)
    snd_wnd: u32,
    /// 相手の MSS(1 つのセグメントに載せるデータの上限)
    peer_mss: u16,
    /// 送るデータ(確認されていない分 + まだ送っていない分)。先頭の番号が `buf_seq`
    send_buf: VecDeque<u8>,
    buf_seq: Seq,
    /// close された(送るデータのあとに FIN を送る)
    fin_queued: bool,
    /// 次に受け取るはずの番号
    rcv_nxt: Seq,
    /// 届いたデータ(まだ読まれていない分)
    recv_buf: VecDeque<u8>,
    /// 相手の FIN を受け取った
    fin_received: bool,
    /// バッファの大きさ(送る・受け取る、どちらも)
    cap: usize,
    /// 最後に知らせたウィンドウ
    adv_wnd: u32,
    /// ACK を送る必要がある(データや FIN を受け取った、おかしなものが届いた)
    ack_pending: bool,
    error: Option<Errno>,
    /// 送ったことのある、いちばん先の番号(これより手前を送るのは再送)
    snd_max: Seq,
    /// 往復時間の推定(RFC 6298 の SRTT と RTTVAR)と、再送の待ち時間
    srtt: Option<u64>,
    rttvar: u64,
    rto: u64,
    /// 測っている最中のセグメント:(その終わりの番号, 送った時刻)。1 度に 1 つだけ測る
    timing: Option<(Seq, u64)>,
    /// 再送タイマー(ウィンドウ 0 の問い合わせにも使う)の時刻と、続けて時間切れになった回数
    rto_at: Option<u64>,
    retries: u32,
    /// 次の dispatch で、ウィンドウが 0 でも 1 バイトだけ送る(問い合わせ)
    probe: bool,
    /// TIME-WAIT が終わる時刻
    time_wait_until: Option<u64>,
    /// 次の dispatch で RST を送る(abort した)
    rst_pending: bool,
}

impl Conn {
    fn new(local: Endpoint, remote: Endpoint, state: State, iss: u32, cap: usize) -> Conn {
        let iss = Seq(iss);
        Conn {
            state,
            local,
            remote,
            iss,
            snd_una: iss,
            snd_nxt: iss,
            snd_wnd: 0,
            peer_mss: DEFAULT_MSS,
            send_buf: VecDeque::new(),
            buf_seq: iss + 1,
            fin_queued: false,
            rcv_nxt: Seq(0),
            recv_buf: VecDeque::new(),
            fin_received: false,
            cap,
            adv_wnd: 0,
            ack_pending: false,
            error: None,
            snd_max: iss,
            srtt: None,
            rttvar: 0,
            rto: INITIAL_RTO,
            timing: None,
            rto_at: None,
            retries: 0,
            probe: false,
            time_wait_until: None,
            rst_pending: false,
        }
    }

    /// 能動オープン(connect)。SYN を送るところから始める。`iss` は最初の番号、`cap` はバッファの大きさ
    pub fn connect(local: Endpoint, remote: Endpoint, iss: u32, cap: usize) -> Conn {
        Conn::new(local, remote, State::SynSent, iss, cap)
    }

    /// 受動オープン:待ち受けているポートに届いた SYN から、SYN-RECEIVED の接続を作る
    pub fn accept(local: Endpoint, remote: Endpoint, syn: &Segment, iss: u32, cap: usize) -> Conn {
        let mut c = Conn::new(local, remote, State::SynReceived, iss, cap);
        c.synchronize(syn);
        c
    }

    /// 相手の SYN から、相手の番号・ウィンドウ・MSS を覚える
    fn synchronize(&mut self, syn: &Segment) {
        self.rcv_nxt = Seq(syn.seq) + 1;
        self.snd_wnd = syn.window as u32;
        self.peer_mss = syn.mss.unwrap_or(DEFAULT_MSS);
    }

    /// 届いたセグメントを処理する(RFC 9293 の 3.10.7 を短くしたもの)
    pub fn on_segment(&mut self, seg: &Segment, now: u64) {
        match self.state {
            State::Closed => {}
            State::SynSent => self.on_segment_syn_sent(seg, now),
            _ => self.on_segment_synchronized(seg, now),
        }
        if self.state == State::TimeWait && self.time_wait_until.is_none() {
            self.time_wait_until = Some(now + TIME_WAIT);
        }
    }

    fn on_segment_synchronized(&mut self, seg: &Segment, now: u64) {
        let seq = Seq(seg.seq);
        // 1. RST:番号が窓の中のものだけ信じる(なりすましの RST で切られないように)
        if seg.has(RST) {
            if self.rcv_nxt <= seq && seq < self.rcv_nxt + self.adv_wnd.max(1) {
                self.state = State::Closed;
                self.error = Some(Errno::ECONNRESET);
            }
            return;
        }
        // 2. 同期したあとの SYN(相手の再送):こちらの答えが届いていない。
        //    SYN-RECEIVED なら SYN-ACK を、つながったあとなら ACK を送り直す
        if seg.has(SYN) {
            if self.state == State::SynReceived {
                self.snd_nxt = self.iss;
            }
            self.ack_pending = true;
            return;
        }
        if !seg.has(ACK) {
            return;
        }
        // 3. 番号を見る。先の番号(あいだが抜けている)は捨て、何番を待っているかを答える。
        //    前に受け取った部分と重なっていれば、重なりを切り落とす
        if seq > self.rcv_nxt {
            self.ack_pending = true;
            return;
        }
        let mut payload = seg.payload;
        let mut fin = seg.has(FIN);
        let skip = (self.rcv_nxt - seq) as usize;
        if skip > 0 {
            self.ack_pending = true; // 同じものがまた来た:相手は ACK を受け取れていない
            if skip > payload.len() {
                fin = false; // FIN も受け取り済み
            }
            payload = &payload[skip.min(payload.len())..];
        }
        // 4. ACK を見る
        if !self.on_ack(Seq(seg.ack), seg.window, now) {
            return;
        }
        // 5. データ
        if !payload.is_empty()
            && matches!(
                self.state,
                State::Established | State::FinWait1 | State::FinWait2
            )
        {
            let take = payload.len().min(self.cap - self.recv_buf.len());
            self.recv_buf.extend(&payload[..take]);
            self.rcv_nxt = self.rcv_nxt + take as u32;
            self.ack_pending = true;
            if take < payload.len() {
                fin = false; // 入りきらなかった。FIN はその先にあるので、まだ受け取れない
            }
        }
        // 6. FIN
        if fin && !self.fin_received {
            self.fin_received = true;
            self.rcv_nxt = self.rcv_nxt + 1;
            self.ack_pending = true;
            self.state = match self.state {
                State::SynReceived | State::Established => State::CloseWait,
                State::FinWait1 if self.fin_acked() => State::TimeWait,
                State::FinWait1 => State::Closing,
                State::FinWait2 => State::TimeWait,
                s => s,
            };
        }
    }

    /// SYN-SENT のとき:相手の SYN(と、こちらの SYN への ACK)を待っている
    fn on_segment_syn_sent(&mut self, seg: &Segment, now: u64) {
        let ack = Seq(seg.ack);
        let ack_ok = seg.has(ACK) && self.iss < ack && ack <= self.snd_nxt;
        if seg.has(ACK) && !ack_ok {
            return;
        }
        if seg.has(RST) {
            if ack_ok {
                // こちらの SYN への答えが RST:そのポートで待っている人がいない
                self.state = State::Closed;
                self.error = Some(Errno::ECONNREFUSED);
            }
            return;
        }
        if !seg.has(SYN) {
            return;
        }
        self.synchronize(seg);
        self.ack_pending = true;
        if ack_ok {
            self.acked(ack, now);
            self.state = State::Established;
        } else {
            // 同時オープン:SYN どうしがすれ違った。SYN を SYN-ACK にして送り直す
            self.state = State::SynReceived;
            self.snd_nxt = self.iss;
        }
    }

    /// ACK の番号を見て、確認された分を送るバッファから捨てる。続きを処理してよければ true
    fn on_ack(&mut self, ack: Seq, window: u16, now: u64) -> bool {
        if self.state == State::SynReceived {
            if !(self.snd_una < ack && ack <= self.snd_nxt) {
                return false;
            }
            self.state = State::Established;
        }
        if ack > self.snd_nxt {
            // まだ送っていないものへの ACK:答えて捨てる
            self.ack_pending = true;
            return false;
        }
        if self.snd_una < ack {
            if ack > self.buf_seq {
                let n = ((ack - self.buf_seq) as usize).min(self.send_buf.len());
                self.send_buf.drain(..n);
                self.buf_seq = self.buf_seq + n as u32;
            }
            self.acked(ack, now);
        }
        if self.snd_una == ack {
            self.snd_wnd = window as u32;
        }
        if self.fin_acked() {
            self.state = match self.state {
                State::FinWait1 => State::FinWait2,
                State::Closing => State::TimeWait,
                State::LastAck => State::Closed,
                s => s,
            };
        }
        true
    }

    /// 新しい ACK が来た:往復時間を測り、再送タイマーを掛け直す
    fn acked(&mut self, ack: Seq, now: u64) {
        self.snd_una = ack;
        self.retries = 0;
        if let Some((end, sent)) = self.timing
            && end <= ack
        {
            self.timing = None;
            self.sample_rtt(now - sent);
        }
        // まだ確認されていないものが残っていれば、そこから測り直す
        self.rto_at = (self.snd_una < self.snd_nxt).then_some(now + self.rto);
    }

    /// 往復時間を 1 つ測った(RFC 6298 の 2)
    fn sample_rtt(&mut self, r: u64) {
        match self.srtt {
            None => {
                self.srtt = Some(r);
                self.rttvar = r / 2;
            }
            Some(srtt) => {
                // RTTVAR = 3/4 RTTVAR + 1/4 |SRTT - R|、SRTT = 7/8 SRTT + 1/8 R
                self.rttvar = (3 * self.rttvar + srtt.abs_diff(r)) / 4;
                self.srtt = Some((7 * srtt + r) / 8);
            }
        }
        let rto = self.srtt.unwrap() + GRANULARITY.max(4 * self.rttvar);
        self.rto = rto.clamp(MIN_RTO, MAX_RTO);
    }

    /// 再送タイマーが切れた
    fn on_timeout(&mut self) {
        self.rto_at = None;
        self.retries += 1;
        let limit = match self.state {
            State::SynSent | State::SynReceived => SYN_RETRIES,
            _ => DATA_RETRIES,
        };
        if self.retries > limit {
            self.state = State::Closed;
            self.error = Some(Errno::ETIMEDOUT);
            return;
        }
        // 待ち時間を倍にする(ネットワークが混んでいるかもしれないので、間をあける)
        self.rto = (self.rto * 2).min(MAX_RTO);
        // Karn:再送したものへの ACK は、最初のものへの答えか再送への答えかわからないので、測らない
        self.timing = None;
        if self.snd_una < self.snd_nxt {
            // 確認されていない最初から送り直す(go-back-N)
            self.snd_nxt = self.snd_una;
        } else {
            // 送ったものは全部確認されているのに、窓が 0 で止まっている:1 バイトで問い合わせる
            self.probe = true;
        }
    }

    /// こちらの FIN が確認された(FIN の番号は、データの最後の次)
    fn fin_acked(&self) -> bool {
        self.fin_queued && self.send_buf.is_empty() && self.snd_una == self.buf_seq + 1
    }

    /// いま送るべきセグメント(TCP のバイト列)を全部取り出す
    pub fn dispatch(&mut self, now: u64) -> Vec<Vec<u8>> {
        if core::mem::take(&mut self.rst_pending) {
            let seq = self.snd_nxt;
            return alloc::vec![self.segment(seq, RST | ACK, None, &[])];
        }
        if self.time_wait_until.is_some_and(|t| now >= t) {
            self.state = State::Closed;
        }
        if self.rto_at.is_some_and(|t| now >= t) && self.state != State::Closed {
            self.on_timeout();
        }
        let out = self.output();
        self.after_output(now);
        out
    }

    /// 送ったあと:測るセグメントを選び、再送タイマー(か、窓が 0 のときの問い合わせのタイマー)を掛ける
    fn after_output(&mut self, now: u64) {
        if self.state == State::Closed {
            self.rto_at = None;
            self.time_wait_until = None;
            return;
        }
        if self.snd_nxt > self.snd_max {
            // 新しい番号まで送った(再送ではない)。1 度に 1 つだけ往復時間を測る
            self.snd_max = self.snd_nxt;
            if self.timing.is_none() {
                self.timing = Some((self.snd_nxt, now));
            }
        }
        let in_flight = self.snd_una < self.snd_nxt;
        let blocked = self.snd_wnd == 0 && self.unsent() > 0;
        if self.rto_at.is_none() && (in_flight || blocked) {
            self.rto_at = Some(now + self.rto);
        }
    }

    /// まだ一度も送っていない(snd_nxt より先の)データの量
    fn unsent(&self) -> usize {
        let data_end = self.buf_seq + self.send_buf.len() as u32;
        if self.snd_nxt < data_end {
            (data_end - self.snd_nxt) as usize
        } else {
            0
        }
    }

    /// いまの変数から、送るものを組み立てる
    fn output(&mut self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        if matches!(self.state, State::SynSent | State::SynReceived) && self.snd_nxt == self.iss {
            let flags = if self.state == State::SynReceived {
                SYN | ACK
            } else {
                SYN
            };
            out.push(self.segment(self.iss, flags, Some(OUR_MSS), &[]));
            self.snd_nxt = self.iss + 1;
        }
        if !matches!(
            self.state,
            State::Closed | State::SynSent | State::SynReceived
        ) {
            loop {
                let data_end = self.buf_seq + self.send_buf.len() as u32;
                // 問い合わせのときだけ、窓が 0 でも 1 バイト送る
                let wnd = if core::mem::take(&mut self.probe) {
                    self.snd_wnd.max(1)
                } else {
                    self.snd_wnd
                };
                let wnd_end = self.snd_una + wnd;
                if self.snd_nxt < data_end && self.snd_nxt < wnd_end {
                    // データ:相手のウィンドウと MSS の小さいほうまで
                    let off = (self.snd_nxt - self.buf_seq) as usize;
                    let n = ((data_end - self.snd_nxt).min(wnd_end - self.snd_nxt) as usize)
                        .min(self.peer_mss as usize);
                    let payload: Vec<u8> = self.send_buf.range(off..off + n).copied().collect();
                    let last = self.snd_nxt + n as u32 == data_end;
                    let mut flags = ACK;
                    if last {
                        flags |= PSH;
                        if self.fin_queued {
                            flags |= FIN; // 最後のデータに FIN を載せる
                        }
                    }
                    let seq = self.snd_nxt;
                    out.push(self.segment(seq, flags, None, &payload));
                    self.snd_nxt = seq + n as u32 + (flags & FIN != 0) as u32;
                } else if self.fin_queued && self.snd_nxt == data_end {
                    let seq = self.snd_nxt;
                    out.push(self.segment(seq, FIN | ACK, None, &[]));
                    self.snd_nxt = seq + 1;
                } else {
                    break;
                }
            }
        }
        if self.ack_pending && out.is_empty() && self.state != State::Closed {
            out.push(self.segment(self.snd_nxt, ACK, None, &[]));
        }
        self.ack_pending = false;
        out
    }

    /// 1 つのセグメントをバイト列にする。ACK の番号とウィンドウは、いまの値を入れる
    fn segment(&mut self, seq: Seq, flags: u8, mss: Option<u16>, payload: &[u8]) -> Vec<u8> {
        self.adv_wnd = ((self.cap - self.recv_buf.len()) as u32).min(u16::MAX as u32);
        Segment {
            src_port: self.local.port,
            dst_port: self.remote.port,
            seq: seq.0,
            ack: self.rcv_nxt.0,
            flags,
            window: self.adv_wnd as u16,
            mss,
            payload,
        }
        .emit(self.local.addr, self.remote.addr)
    }

    /// まだ書ける(close していない、切れていない)
    pub fn may_send(&self) -> bool {
        !self.fin_queued && !matches!(self.state, State::Closed | State::TimeWait)
    }

    /// 送るデータを積む。積めた数を返す(バッファがいっぱいなら 0)
    pub fn send(&mut self, data: &[u8]) -> usize {
        if self.fin_queued || self.state == State::Closed {
            return 0;
        }
        let n = data.len().min(self.cap - self.send_buf.len());
        self.send_buf.extend(&data[..n]);
        n
    }

    /// 届いたデータを読む
    pub fn recv(&mut self, buf: &mut [u8]) -> usize {
        let n = buf.len().min(self.recv_buf.len());
        for (dst, src) in buf.iter_mut().zip(self.recv_buf.drain(..n)) {
            *dst = src;
        }
        // 知らせたウィンドウが小さかったなら、開いたことを知らせる(相手が送るのを止めているかもしれない)
        if n > 0 && (self.adv_wnd as usize) < self.cap / 2 {
            self.ack_pending = true;
        }
        n
    }

    /// 読めるデータの量
    pub fn readable(&self) -> usize {
        self.recv_buf.len()
    }

    /// 相手が FIN を送ってきて、もう読むものが来ない
    pub fn is_eof(&self) -> bool {
        self.fin_received && self.recv_buf.is_empty()
    }

    /// 送るほうを閉じる(FIN を、積んだデータのあとに送る)
    pub fn close(&mut self) {
        match self.state {
            State::SynSent => self.state = State::Closed,
            State::SynReceived | State::Established => self.state = State::FinWait1,
            State::CloseWait => self.state = State::LastAck,
            _ => return, // もう閉じている
        }
        self.fin_queued = true;
    }

    /// RST を送って、すぐに終わる(待ち受けをやめたときの、accept されていない接続など)
    pub fn abort(&mut self) {
        if !matches!(self.state, State::Closed | State::SynSent) {
            self.rst_pending = true;
        }
        self.state = State::Closed;
        self.rto_at = None;
        self.time_wait_until = None;
    }

    /// RST で終わったときの理由(ECONNREFUSED / ECONNRESET)、再送をあきらめたとき(ETIMEDOUT)
    pub fn error(&self) -> Option<Errno> {
        self.error
    }

    /// 次にタイマーで `dispatch` を呼んでほしい時刻(ナノ秒)。タイマーがなければ None
    pub fn poll_at(&self) -> Option<u64> {
        match (self.rto_at, self.time_wait_until) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// いまの再送の待ち時間(ナノ秒)
    pub fn rto(&self) -> u64 {
        self.rto
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const CLIENT: Endpoint = Endpoint {
        addr: Ipv4Addr::new(10, 0, 2, 15),
        port: 49152,
    };
    const SERVER: Endpoint = Endpoint {
        addr: Ipv4Addr::new(10, 0, 2, 2),
        port: 80,
    };
    const CAP: usize = 4096;

    /// from が送ったもの(線の上で見えるもの)。時刻は 0
    fn sent(from: &mut Conn) -> Vec<Vec<u8>> {
        from.dispatch(0)
    }

    fn parse<'a>(from: &Conn, bytes: &'a [u8]) -> Segment<'a> {
        Segment::parse(from.local.addr, from.remote.addr, bytes).unwrap()
    }

    /// from が送るものを全部 to に渡す。渡した数を返す
    fn deliver_at(from: &mut Conn, to: &mut Conn, now: u64) -> usize {
        let out = from.dispatch(now);
        for bytes in &out {
            to.on_segment(&parse(from, bytes), now);
        }
        out.len()
    }

    /// 送るものがなくなるまで、2 つのあいだでセグメントを渡し合う
    fn exchange(a: &mut Conn, b: &mut Conn) {
        exchange_at(a, b, 0)
    }

    fn exchange_at(a: &mut Conn, b: &mut Conn, now: u64) {
        while deliver_at(a, b, now) + deliver_at(b, a, now) > 0 {}
    }

    /// 3 ウェイハンドシェイクを済ませた 2 つ
    fn established() -> (Conn, Conn) {
        let mut client = Conn::connect(CLIENT, SERVER, 1000, CAP);
        let syn = sent(&mut client);
        let mut server = Conn::accept(SERVER, CLIENT, &parse(&client, &syn[0]), 5000, CAP);
        exchange(&mut client, &mut server);
        (client, server)
    }

    #[test]
    fn three_way_handshake() {
        let mut client = Conn::connect(CLIENT, SERVER, 1000, CAP);
        assert_eq!(client.state, State::SynSent);
        let out = sent(&mut client);
        assert_eq!(out.len(), 1);
        let syn = parse(&client, &out[0]);
        assert_eq!((syn.flags, syn.seq, syn.mss), (SYN, 1000, Some(OUR_MSS)));
        assert_eq!((syn.src_port, syn.dst_port), (49152, 80));

        let mut server = Conn::accept(SERVER, CLIENT, &syn, 5000, CAP);
        assert_eq!(server.state, State::SynReceived);
        let out = sent(&mut server);
        let syn_ack = parse(&server, &out[0]);
        assert_eq!(
            (syn_ack.flags, syn_ack.seq, syn_ack.ack),
            (SYN | ACK, 5000, 1001)
        );
        assert_eq!(syn_ack.mss, Some(OUR_MSS));

        client.on_segment(&syn_ack, 0);
        assert_eq!(client.state, State::Established);
        let out = sent(&mut client);
        let ack = parse(&client, &out[0]);
        assert_eq!((ack.flags, ack.seq, ack.ack), (ACK, 1001, 5001));
        server.on_segment(&ack, 0);
        assert_eq!(server.state, State::Established);
        // 送るものがなければ、何も出ない
        assert!(sent(&mut client).is_empty() && sent(&mut server).is_empty());
    }

    #[test]
    fn data_flows_both_ways() {
        let (mut client, mut server) = established();
        assert_eq!(client.send(b"GET / HTTP/1.0\r\n\r\n"), 18);
        exchange(&mut client, &mut server);
        let mut buf = [0; 64];
        assert_eq!(server.readable(), 18);
        assert_eq!(server.recv(&mut buf), 18);
        assert_eq!(&buf[..18], b"GET / HTTP/1.0\r\n\r\n");
        assert_eq!(server.readable(), 0);

        server.send(b"HTTP/1.0 200 OK\r\n");
        exchange(&mut client, &mut server);
        let n = client.recv(&mut buf);
        assert_eq!(&buf[..n], b"HTTP/1.0 200 OK\r\n");
    }

    #[test]
    fn segments_are_cut_at_the_peers_mss() {
        let (mut client, mut server) = established();
        let data = vec![7u8; 3000];
        assert_eq!(client.send(&data), 3000);
        let out = sent(&mut client);
        let lens: Vec<usize> = out
            .iter()
            .map(|b| parse(&client, b).payload.len())
            .collect();
        assert_eq!(lens, [1460, 1460, 80]);
        for b in &out {
            server.on_segment(&parse(&client, b), 0);
        }
        assert_eq!(server.readable(), 3000);
    }

    #[test]
    fn sender_stops_at_the_receivers_window_and_resumes_after_a_read() {
        let mut client = Conn::connect(CLIENT, SERVER, 1000, 8192);
        let syn = sent(&mut client);
        // サーバーのバッファは 1000 バイトだけ
        let mut server = Conn::accept(SERVER, CLIENT, &parse(&client, &syn[0]), 5000, 1000);
        exchange(&mut client, &mut server);
        assert_eq!(client.send(&[1u8; 3000]), 3000);
        exchange(&mut client, &mut server);
        assert_eq!(server.readable(), 1000);
        // 読むとウィンドウが開く。開いたことを知らせる ACK で、続きが来る
        let mut buf = [0; 1000];
        server.recv(&mut buf);
        exchange(&mut client, &mut server);
        assert_eq!(server.readable(), 1000);
        server.recv(&mut buf);
        server.recv(&mut buf);
        exchange(&mut client, &mut server);
        server.recv(&mut buf);
        exchange(&mut client, &mut server);
        assert_eq!(server.readable(), 0);
        assert_eq!(client.send(&[]), 0);
    }

    #[test]
    fn send_buffer_is_bounded() {
        let (mut client, _server) = established();
        assert_eq!(client.send(&[0; CAP + 10]), CAP);
        assert_eq!(client.send(&[0; 1]), 0);
    }

    #[test]
    fn active_close_goes_through_fin_wait_to_time_wait() {
        let (mut client, mut server) = established();
        client.send(b"bye");
        client.close();
        assert_eq!(client.state, State::FinWait1);
        // データのあとに FIN
        let out = sent(&mut client);
        let last = parse(&client, out.last().unwrap());
        assert!(last.has(FIN));
        assert_eq!(last.payload, b"bye");
        server.on_segment(&last, 0);
        assert_eq!(server.state, State::CloseWait);
        assert!(!server.is_eof(), "まだ読んでいないデータがある");
        let mut buf = [0; 8];
        assert_eq!(server.recv(&mut buf), 3);
        assert!(server.is_eof());

        for b in sent(&mut server) {
            client.on_segment(&parse(&server, &b), 0);
        }
        assert_eq!(client.state, State::FinWait2);
        server.close();
        assert_eq!(server.state, State::LastAck);
        exchange(&mut client, &mut server);
        assert_eq!(client.state, State::TimeWait);
        assert_eq!(server.state, State::Closed);
        assert!(client.is_eof());
    }

    #[test]
    fn simultaneous_close_goes_through_closing() {
        let (mut client, mut server) = established();
        client.close();
        server.close();
        let from_client = sent(&mut client);
        let from_server = sent(&mut server);
        client.on_segment(&parse(&server, &from_server[0]), 0);
        server.on_segment(&parse(&client, &from_client[0]), 0);
        assert_eq!(client.state, State::Closing);
        assert_eq!(server.state, State::Closing);
        exchange(&mut client, &mut server);
        assert_eq!(client.state, State::TimeWait);
        assert_eq!(server.state, State::TimeWait);
    }

    #[test]
    fn rst_in_reply_to_syn_is_connection_refused() {
        let mut client = Conn::connect(CLIENT, SERVER, 1000, CAP);
        sent(&mut client);
        let rst = Segment {
            src_port: 80,
            dst_port: 49152,
            seq: 0,
            ack: 1001,
            flags: RST | ACK,
            window: 0,
            mss: None,
            payload: &[],
        };
        client.on_segment(&rst, 0);
        assert_eq!(client.state, State::Closed);
        assert_eq!(client.error(), Some(Errno::ECONNREFUSED));
    }

    #[test]
    fn rst_on_an_established_connection_is_reset_but_only_in_window() {
        let (mut client, _server) = established();
        let mut rst = Segment {
            src_port: 80,
            dst_port: 49152,
            seq: 9999, // 窓の外(なりすましの RST)は無視する
            ack: 0,
            flags: RST,
            window: 0,
            mss: None,
            payload: &[],
        };
        client.on_segment(&rst, 0);
        assert_eq!(client.state, State::Established);
        rst.seq = 5001;
        client.on_segment(&rst, 0);
        assert_eq!(client.state, State::Closed);
        assert_eq!(client.error(), Some(Errno::ECONNRESET));
    }

    #[test]
    fn out_of_order_data_is_dropped_with_a_duplicate_ack_and_overlap_is_trimmed() {
        let (mut client, mut server) = established();
        client.send(b"hello world");
        let out = sent(&mut client);
        let seg = parse(&client, &out[0]);
        // 先の部分だけ(" world")が先に届いた:捨てて、まだ 1001 を待っていると答える
        let later = Segment {
            seq: seg.seq + 5,
            payload: &seg.payload[5..],
            ..seg.clone()
        };
        server.on_segment(&later, 0);
        assert_eq!(server.readable(), 0);
        let dup = sent(&mut server);
        let ack = parse(&server, &dup[0]);
        assert_eq!((ack.flags, ack.ack), (ACK, 1001));
        // 前半("hello")が届く
        let first = Segment {
            payload: &seg.payload[..5],
            ..seg.clone()
        };
        server.on_segment(&first, 0);
        // 全体が再送されて届く:重なっている前半は捨て、後半だけ足す
        server.on_segment(&seg, 0);
        let mut buf = [0; 32];
        let n = server.recv(&mut buf);
        assert_eq!(&buf[..n], b"hello world");
        // 同じものがもう一度届いても、増えない
        server.on_segment(&seg, 0);
        assert_eq!(server.readable(), 0);
    }

    #[test]
    fn acks_beyond_what_was_sent_are_ignored() {
        let (mut client, mut server) = established();
        server.send(b"abc");
        let bogus = Segment {
            src_port: 49152,
            dst_port: 80,
            seq: 1001,
            ack: 9000,
            flags: ACK,
            window: 4096,
            mss: None,
            payload: &[],
        };
        server.on_segment(&bogus, 0);
        // まだ確認されていないので、送る
        exchange(&mut client, &mut server);
        let mut buf = [0; 8];
        assert_eq!(client.recv(&mut buf), 3);
    }

    const MS: u64 = 1_000_000;
    const SEC: u64 = 1_000 * MS;

    #[test]
    fn a_lost_segment_is_sent_again_after_the_rto() {
        let (mut client, mut server) = established();
        // ハンドシェイクで往復時間を測っている(時刻 0 で往復したので、下限の 200ms)
        let rto = client.rto();
        assert_eq!(rto, MIN_RTO);
        client.send(b"lost");
        let first = client.dispatch(0); // 線の上で失われる
        assert_eq!(client.poll_at(), Some(rto));
        assert!(client.dispatch(rto - 1).is_empty(), "まだ時間前");
        let again = client.dispatch(rto);
        assert_eq!(again, first, "同じものを送り直す");
        server.on_segment(&parse(&client, &again[0]), rto);
        deliver_at(&mut server, &mut client, rto);
        assert_eq!(server.readable(), 4);
        // 全部確認されたら、タイマーは止まる
        assert_eq!(client.poll_at(), None);
    }

    #[test]
    fn the_rto_doubles_each_time_and_gives_up_with_etimedout() {
        let mut client = Conn::connect(CLIENT, SERVER, 1000, CAP);
        let mut at = 0;
        let mut sent_at = Vec::new();
        while client.state != State::Closed {
            let out = client.dispatch(at);
            if !out.is_empty() {
                assert!(parse(&client, &out[0]).has(SYN));
                sent_at.push(at / SEC);
            }
            match client.poll_at() {
                Some(t) => at = t,
                None => break,
            }
        }
        // 1, 2, 4, 8, 16 秒と間を倍にして、SYN を 6 回送る。最後の待ちが切れたらあきらめる
        assert_eq!(sent_at, [0, 1, 3, 7, 15, 31]);
        assert_eq!(client.state, State::Closed);
        assert_eq!(client.error(), Some(Errno::ETIMEDOUT));
        assert_eq!(at / SEC, 63);
    }

    #[test]
    fn round_trip_times_set_the_rto() {
        let (mut client, mut server) = established();
        // 往復 300ms の線で何度もやりとりすると、RTO は 300ms 近くに落ち着く(ばらつきがないので)
        let mut now = 0;
        for _ in 0..60 {
            client.send(b"ping");
            let out = client.dispatch(now);
            server.on_segment(&parse(&client, &out[0]), now + 150 * MS);
            let ack = server.dispatch(now + 150 * MS);
            client.on_segment(&parse(&server, &ack[0]), now + 300 * MS);
            now += SEC;
        }
        let rto = client.rto();
        assert!((300 * MS..400 * MS).contains(&rto), "rto = {rto}");
        // とても速い線でも、下限より短くはしない
        for _ in 0..50 {
            client.send(b"ping");
            let out = client.dispatch(now);
            server.on_segment(&parse(&client, &out[0]), now);
            let ack = server.dispatch(now);
            client.on_segment(&parse(&server, &ack[0]), now + MS);
            now += SEC;
        }
        assert_eq!(client.rto(), MIN_RTO);
    }

    #[test]
    fn acks_for_retransmitted_segments_are_not_timed() {
        // Karn のアルゴリズム:再送したものへの ACK は、どちらへの答えかわからないので測らない
        let (mut client, mut server) = established();
        let rto = client.rto();
        client.send(b"x");
        client.dispatch(0); // 失われる
        let again = client.dispatch(rto);
        server.on_segment(&parse(&client, &again[0]), rto);
        let ack = server.dispatch(rto);
        client.on_segment(&parse(&server, &ack[0]), rto + MS);
        // 測っていれば、送り直してから 1ms で答えが来たので、待ち時間が縮む。倍にしたまま
        assert_eq!(client.rto(), 2 * rto);
    }

    #[test]
    fn a_lost_fin_is_sent_again() {
        let (mut client, mut server) = established();
        client.close();
        let fin = client.dispatch(0);
        assert!(parse(&client, &fin[0]).has(FIN));
        let again = client.dispatch(INITIAL_RTO);
        assert_eq!(again, fin);
        server.on_segment(&parse(&client, &again[0]), INITIAL_RTO);
        exchange_at(&mut client, &mut server, INITIAL_RTO);
        assert_eq!(client.state, State::FinWait2);
    }

    #[test]
    fn a_lost_syn_ack_is_sent_again() {
        let mut client = Conn::connect(CLIENT, SERVER, 1000, CAP);
        let syn = client.dispatch(0);
        let mut server = Conn::accept(SERVER, CLIENT, &parse(&client, &syn[0]), 5000, CAP);
        let syn_ack = server.dispatch(0); // 失われる
        // クライアントも SYN を送り直す。サーバーはそれに ACK だけでなく、SYN-ACK を送り直す
        let syn_again = client.dispatch(INITIAL_RTO);
        assert_eq!(syn_again, syn);
        server.on_segment(&parse(&client, &syn_again[0]), INITIAL_RTO);
        let again = server.dispatch(INITIAL_RTO);
        assert_eq!(again, syn_ack);
        client.on_segment(&parse(&server, &again[0]), INITIAL_RTO);
        exchange_at(&mut client, &mut server, INITIAL_RTO);
        assert_eq!(
            (client.state, server.state),
            (State::Established, State::Established)
        );
    }

    #[test]
    fn a_zero_window_is_probed_when_the_window_update_is_lost() {
        let mut client = Conn::connect(CLIENT, SERVER, 1000, CAP);
        let syn = client.dispatch(0);
        let mut server = Conn::accept(SERVER, CLIENT, &parse(&client, &syn[0]), 5000, 100);
        exchange(&mut client, &mut server);
        client.send(&[9; 300]);
        exchange(&mut client, &mut server);
        assert_eq!(server.readable(), 100); // 窓は 0 になった
        let mut buf = [0; 100];
        server.recv(&mut buf);
        server.dispatch(0); // 「開いた」の ACK が失われる
        // クライアントは、しばらくして 1 バイトだけ送って様子を見る
        let at = client.poll_at().expect("persist timer");
        let probe = client.dispatch(at);
        assert_eq!(parse(&client, &probe[0]).payload.len(), 1);
        server.on_segment(&parse(&client, &probe[0]), at);
        exchange_at(&mut client, &mut server, at);
        assert_eq!(server.readable(), 100);
    }

    #[test]
    fn time_wait_ends_after_two_msl() {
        let (mut client, mut server) = established();
        client.close();
        exchange_at(&mut client, &mut server, 0);
        server.close();
        exchange_at(&mut client, &mut server, 0);
        assert_eq!(client.state, State::TimeWait);
        assert_eq!(client.poll_at(), Some(TIME_WAIT));
        client.dispatch(TIME_WAIT - 1);
        assert_eq!(client.state, State::TimeWait);
        client.dispatch(TIME_WAIT);
        assert_eq!(client.state, State::Closed);
        assert_eq!(client.poll_at(), None);
    }

    #[test]
    fn an_idle_connection_has_no_timer() {
        let (client, server) = established();
        assert_eq!((client.poll_at(), server.poll_at()), (None, None));
    }
}
