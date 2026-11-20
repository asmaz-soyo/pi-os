//! パイプ(pipe2)のテスト。

use super::tests::{SCRATCH, call, err, get, process, put};
use super::*;
use crate::Errno;

const O_CLOEXEC: u64 = 0o2000000;
const F_GETFD: u64 = 1;

/// pipe2 を呼び、(読む側, 書く側) を返す
fn pipe(p: &mut crate::process::Process, flags: u64) -> (u64, u64) {
    assert_eq!(call(p, nr::PIPE2, [SCRATCH, flags, 0, 0, 0, 0]), 0);
    let b = get(p, SCRATCH, 8);
    let r = i32::from_le_bytes(b[..4].try_into().unwrap()) as u64;
    let w = i32::from_le_bytes(b[4..].try_into().unwrap()) as u64;
    (r, w)
}

#[test]
fn bytes_written_to_a_pipe_are_read_back() {
    let (mut p, _) = process();
    let (r, w) = pipe(&mut p, 0);
    assert_eq!((r, w), (3, 4));
    put(&mut p, SCRATCH + 0x100, b"through the pipe");
    assert_eq!(
        call(&mut p, nr::WRITE, [w, SCRATCH + 0x100, 16, 0, 0, 0]),
        16
    );
    // 少しずつ読める
    assert_eq!(call(&mut p, nr::READ, [r, SCRATCH + 0x200, 7, 0, 0, 0]), 7);
    assert_eq!(get(&mut p, SCRATCH + 0x200, 7), b"through");
    assert_eq!(
        call(&mut p, nr::READ, [r, SCRATCH + 0x200, 100, 0, 0, 0]),
        9
    );
    assert_eq!(get(&mut p, SCRATCH + 0x200, 9), b" the pipe");
}

#[test]
fn reading_after_all_writers_close_is_end_of_file() {
    let (mut p, _) = process();
    let (r, w) = pipe(&mut p, 0);
    put(&mut p, SCRATCH + 0x100, b"last");
    call(&mut p, nr::WRITE, [w, SCRATCH + 0x100, 4, 0, 0, 0]);
    // 書く側を複製してから両方閉じる(最後の 1 つが閉じたときに終わりになる)
    let w2 = call(&mut p, nr::DUP, [w, 0, 0, 0, 0, 0]);
    call(&mut p, nr::CLOSE, [w, 0, 0, 0, 0, 0]);
    call(&mut p, nr::CLOSE, [w2, 0, 0, 0, 0, 0]);
    assert_eq!(
        call(&mut p, nr::READ, [r, SCRATCH + 0x200, 100, 0, 0, 0]),
        4
    );
    assert_eq!(
        call(&mut p, nr::READ, [r, SCRATCH + 0x200, 100, 0, 0, 0]),
        0
    );
}

#[test]
fn writing_without_readers_is_epipe() {
    let (mut p, _) = process();
    let (r, w) = pipe(&mut p, 0);
    call(&mut p, nr::CLOSE, [r, 0, 0, 0, 0, 0]);
    assert_eq!(
        call(&mut p, nr::WRITE, [w, SCRATCH, 1, 0, 0, 0]),
        err(Errno::EPIPE)
    );
}

#[test]
fn the_ends_are_one_way_and_not_seekable() {
    let (mut p, _) = process();
    let (r, w) = pipe(&mut p, 0);
    assert_eq!(
        call(&mut p, nr::WRITE, [r, SCRATCH, 1, 0, 0, 0]),
        err(Errno::EBADF)
    );
    assert_eq!(
        call(&mut p, nr::READ, [w, SCRATCH, 1, 0, 0, 0]),
        err(Errno::EBADF)
    );
    assert_eq!(
        call(&mut p, nr::LSEEK, [r, 0, 0, 0, 0, 0]),
        err(Errno::ESPIPE)
    );
}

#[test]
fn pipe2_honours_o_cloexec() {
    let (mut p, _) = process();
    let (r, w) = pipe(&mut p, O_CLOEXEC);
    assert_eq!(call(&mut p, nr::FCNTL, [r, F_GETFD, 0, 0, 0, 0]), 1);
    assert_eq!(call(&mut p, nr::FCNTL, [w, F_GETFD, 0, 0, 0, 0]), 1);
    assert_eq!(
        call(&mut p, nr::PIPE2, [SCRATCH, 0x1, 0, 0, 0, 0]),
        err(Errno::EINVAL)
    );
}
