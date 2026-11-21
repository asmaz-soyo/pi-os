//! パイプ:片方から書いたバイトを、もう片方から読む(`a | b`)。
//!
//! 読む側と書く側は別の `File` で、どちらも同じ `Pipe`(バッファ)を指す。fd を dup したり
//! fork したりすると同じ端が共有され、最後の 1 つが閉じられたとき(`Drop`)に「閉じた」と記録する。
//! - 書く側がすべて閉じた:読むと、残りを読み切ったあと 0(ファイルの終わり)
//! - 読む側がすべて閉じた:書くと `EPIPE`(Linux ならここで SIGPIPE も届く)

use crate::Errno;
use crate::fs::vfs::{File, FileKind, Stat};
use crate::sync::SpinLock;
use alloc::collections::VecDeque;
use alloc::sync::Arc;

/// バッファの大きさ(Linux と同じ 64KiB)
const CAPACITY: usize = 64 * 1024;

#[derive(Default)]
struct State {
    buf: VecDeque<u8>,
    reader_closed: bool,
    writer_closed: bool,
}

#[derive(Default)]
struct Pipe {
    state: SpinLock<State>,
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

/// パイプを作り、(読む側, 書く側) を返す。
pub fn new() -> (Arc<dyn File>, Arc<dyn File>) {
    let pipe = Arc::new(Pipe::default());
    (Arc::new(ReadEnd(pipe.clone())), Arc::new(WriteEnd(pipe)))
}

impl File for ReadEnd {
    fn stat(&self) -> Stat {
        stat()
    }

    fn read(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            {
                let mut s = self.0.state.lock();
                if !s.buf.is_empty() {
                    let n = buf.len().min(s.buf.len());
                    for (dst, src) in buf.iter_mut().zip(s.buf.drain(..n)) {
                        *dst = src;
                    }
                    return Ok(n);
                }
                if s.writer_closed {
                    return Ok(0);
                }
            }
            // まだ何もない:書かれるのを待つ
            crate::sched::relax();
        }
    }

    fn poll_readable(&self) -> bool {
        let s = self.0.state.lock();
        !s.buf.is_empty() || s.writer_closed
    }
}

impl Drop for ReadEnd {
    fn drop(&mut self) {
        self.0.state.lock().reader_closed = true;
    }
}

impl File for WriteEnd {
    fn stat(&self) -> Stat {
        stat()
    }

    fn write(&self, _offset: u64, buf: &[u8]) -> Result<usize, Errno> {
        let mut done = 0;
        while done < buf.len() {
            {
                let mut s = self.0.state.lock();
                if s.reader_closed {
                    // 少しでも書けていればその分を返す
                    return if done > 0 {
                        Ok(done)
                    } else {
                        Err(Errno::EPIPE)
                    };
                }
                let room = CAPACITY - s.buf.len();
                let n = room.min(buf.len() - done);
                s.buf.extend(&buf[done..done + n]);
                done += n;
            }
            if done < buf.len() {
                // いっぱい:読まれるのを待つ
                crate::sched::relax();
            }
        }
        Ok(done)
    }
}

impl Drop for WriteEnd {
    fn drop(&mut self) {
        self.0.state.lock().writer_closed = true;
    }
}
