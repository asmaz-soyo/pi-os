//! パイプ:片方から書いたバイトを、もう片方から読む(`a | b`)。
//!
//! 読む側と書く側は別の `File` で、どちらも同じ `Pipe`(バッファ)を指す。fd を dup したり
//! fork したりすると同じ端が共有され、最後の 1 つが閉じられたとき(`Drop`)に「閉じた」と記録する。
//! - 書く側がすべて閉じた:読むと、残りを読み切ったあと 0(ファイルの終わり)
//! - 読む側がすべて閉じた:書くと `EPIPE`(Linux ならここで SIGPIPE も届く)
//!
//! 待つ(空で読む、いっぱいで書く、FIFO の相手を待つ)ときは、パイプの待ち行列で眠る。
//! 状態を変えた側(書いた、読んだ、開いた、閉じた)が起こす。

use crate::Errno;
use crate::fs::vfs::{File, FileKind, Stat};
use crate::sync::SpinLock;
use crate::wait::{WaitQueue, wait_event};
use alloc::collections::VecDeque;
use alloc::sync::Arc;

/// バッファの大きさ(Linux と同じ 64KiB)
const CAPACITY: usize = 64 * 1024;

#[derive(Default)]
struct State {
    buf: VecDeque<u8>,
    /// 開いている読む側・書く側の数(名前付きパイプは、何人も開いたり閉じたりする)
    readers: usize,
    writers: usize,
}

#[derive(Default)]
struct Pipe {
    state: SpinLock<State>,
    /// 読む側・書く側の両方が待つ(状態が変わったら全員起こし、それぞれが条件を見直す)
    wq: WaitQueue,
}

fn stat() -> Stat {
    Stat {
        kind: FileKind::Fifo,
        mode: 0o600,
        size: 0,
        ino: 0x3_0000,
        uid: 0,
        gid: 0,
        ..Default::default()
    }
}

pub struct ReadEnd(Arc<Pipe>);
pub struct WriteEnd(Arc<Pipe>);

impl ReadEnd {
    fn open(pipe: Arc<Pipe>) -> Self {
        pipe.state.lock().readers += 1;
        pipe.wq.wake_all(); // 読む側を待っている FIFO の書く側
        Self(pipe)
    }
}

impl WriteEnd {
    fn open(pipe: Arc<Pipe>) -> Self {
        pipe.state.lock().writers += 1;
        pipe.wq.wake_all();
        Self(pipe)
    }
}

/// パイプを作り、(読む側, 書く側) を返す。
pub fn new() -> (Arc<dyn File>, Arc<dyn File>) {
    let (r, w) = ends();
    (Arc::new(r), Arc::new(w))
}

/// パイプを作り、両端をそのままの型で返す(ほかの `File` の部品にするため。ソケットなど)
pub fn ends() -> (ReadEnd, WriteEnd) {
    let pipe = Arc::new(Pipe::default());
    (ReadEnd::open(pipe.clone()), WriteEnd::open(pipe))
}

/// 名前付きパイプ(FIFO)の中身:(st_dev, inode 番号) → パイプ。開いている人がいる間だけ残る
static FIFOS: crate::sync::SpinLock<
    alloc::collections::BTreeMap<(u64, u64), alloc::sync::Weak<Pipe>>,
> = crate::sync::SpinLock::new(alloc::collections::BTreeMap::new());

/// 名前付きパイプを開く。同じ FIFO(`key` = (st_dev, inode 番号))を開いた人どうしがつながる。
/// 読む側は書く側が来るまで、書く側は読む側が来るまで待つ。`nonblock` なら待たない
/// (読む側はそのまま開き、書く側は読む側がいなければ ENXIO)。
pub fn open_fifo(key: (u64, u64), write: bool, nonblock: bool) -> Result<Arc<dyn File>, Errno> {
    let pipe = {
        let mut fifos = FIFOS.lock();
        fifos.retain(|_, w| w.strong_count() > 0);
        match fifos.get(&key).and_then(|w| w.upgrade()) {
            Some(p) => p,
            None => {
                let p = Arc::new(Pipe::default());
                fifos.insert(key, Arc::downgrade(&p));
                p
            }
        }
    };
    if write {
        if nonblock && pipe.state.lock().readers == 0 {
            return Err(Errno::ENXIO);
        }
        let end = WriteEnd::open(pipe.clone());
        wait_event(&[&pipe.wq], None, || {
            (pipe.state.lock().readers > 0).then_some(())
        });
        Ok(Arc::new(end))
    } else {
        let end = ReadEnd::open(pipe.clone());
        if !nonblock {
            wait_event(&[&pipe.wq], None, || {
                (pipe.state.lock().writers > 0).then_some(())
            });
        }
        Ok(Arc::new(end))
    }
}

impl File for ReadEnd {
    fn stat(&self) -> Stat {
        stat()
    }

    fn read(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        if buf.is_empty() {
            return Ok(0);
        }
        // まだ何もなければ、書かれる(か、書く側が全部閉じる)のを眠って待つ
        let n = wait_event(&[&self.0.wq], None, || {
            let mut s = self.0.state.lock();
            if !s.buf.is_empty() {
                let n = buf.len().min(s.buf.len());
                for (dst, src) in buf.iter_mut().zip(s.buf.drain(..n)) {
                    *dst = src;
                }
                return Some(n);
            }
            (s.writers == 0).then_some(0)
        });
        // 空きができた:いっぱいで待っている書く側を起こす
        self.0.wq.wake_all();
        Ok(n.unwrap_or(0))
    }

    fn poll_readable(&self) -> bool {
        let s = self.0.state.lock();
        !s.buf.is_empty() || s.writers == 0
    }
}

impl Drop for ReadEnd {
    fn drop(&mut self) {
        self.0.state.lock().readers -= 1;
        self.0.wq.wake_all(); // 書く側は EPIPE に
    }
}

impl File for WriteEnd {
    fn stat(&self) -> Stat {
        stat()
    }

    fn write(&self, _offset: u64, buf: &[u8]) -> Result<usize, Errno> {
        let mut done = 0;
        // 書けるだけ書き、いっぱいなら読まれるのを眠って待つ
        let r = wait_event(&[&self.0.wq], None, || {
            let mut s = self.0.state.lock();
            if s.readers == 0 {
                // 少しでも書けていればその分を返す
                return Some(if done > 0 {
                    Ok(done)
                } else {
                    Err(Errno::EPIPE)
                });
            }
            let n = (CAPACITY - s.buf.len()).min(buf.len() - done);
            s.buf.extend(&buf[done..done + n]);
            done += n;
            if n > 0 {
                drop(s);
                self.0.wq.wake_all(); // 読む側を起こす
            }
            (done == buf.len()).then_some(Ok(done))
        });
        r.unwrap_or(Ok(done))
    }
}

impl Drop for WriteEnd {
    fn drop(&mut self) {
        self.0.state.lock().writers -= 1;
        self.0.wq.wake_all(); // 読む側は終わり(0)を読む
    }
}
