//! 端末(stdin)、termios、select / poll のテスト。

use super::tests::{SCRATCH, call, get, open, process, put};
use super::*;
use crate::tty::{ECHO, ICANON, TERMIOS_SIZE, Termios};

const TCGETS: u64 = 0x5401;
const TCSETS: u64 = 0x5402;

#[test]
fn reading_stdin_returns_a_line_and_echoes_it() {
    let (mut p, out) = process();
    out.type_in(b"hi\r");
    assert_eq!(call(&mut p, nr::READ, [0, SCRATCH, 10, 0, 0, 0]), 3);
    assert_eq!(get(&mut p, SCRATCH, 3), b"hi\n");
    assert_eq!(&out.0.lock()[..], b"hi\n", "echo");
}

#[test]
fn ctrl_d_on_an_empty_line_reads_as_end_of_file() {
    let (mut p, out) = process();
    out.type_in(b"\x04");
    assert_eq!(call(&mut p, nr::READ, [0, SCRATCH, 10, 0, 0, 0]), 0);
}

#[test]
fn termios_can_be_read_and_switched_to_raw_mode() {
    let (mut p, out) = process();
    assert_eq!(call(&mut p, nr::IOCTL, [0, TCGETS, SCRATCH, 0, 0, 0]), 0);
    let bytes: [u8; TERMIOS_SIZE] = get(&mut p, SCRATCH, TERMIOS_SIZE).try_into().unwrap();
    let mut t = Termios::from_bytes(&bytes);
    assert_ne!(t.lflag & ICANON, 0);
    t.lflag &= !(ICANON | ECHO);
    put(&mut p, SCRATCH, &t.to_bytes());
    assert_eq!(call(&mut p, nr::IOCTL, [0, TCSETS, SCRATCH, 0, 0, 0]), 0);
    out.type_in(b"x");
    assert_eq!(call(&mut p, nr::READ, [0, SCRATCH + 0x100, 10, 0, 0, 0]), 1);
    assert_eq!(get(&mut p, SCRATCH + 0x100, 1), b"x");
    assert!(out.0.lock().is_empty(), "no echo in raw mode");
}

#[test]
fn termios_ioctls_on_regular_files_are_enotty() {
    let (mut p, _) = process();
    let fd = open(&mut p, b"/etc/motd", 0);
    assert_eq!(
        call(&mut p, nr::IOCTL, [fd, TCGETS, SCRATCH, 0, 0, 0]),
        crate::Errno::ENOTTY.as_syscall_return()
    );
}

/// fd_set(64 ビットの語の並び)を作る
fn fdset(p: &mut crate::process::Process, va: u64, fds: &[u64]) {
    let mut word = 0u64;
    for fd in fds {
        word |= 1 << fd;
    }
    put(p, va, &word.to_le_bytes());
}

fn fdset_get(p: &mut crate::process::Process, va: u64) -> u64 {
    u64::from_le_bytes(get(p, va, 8).try_into().unwrap())
}

#[test]
fn pselect_reports_stdin_only_when_input_is_ready() {
    let (mut p, out) = process();
    let (rd, wr, tmo) = (SCRATCH, SCRATCH + 0x10, SCRATCH + 0x20);
    put(&mut p, tmo, &[0u8; 16]); // 待たない
    fdset(&mut p, rd, &[0]);
    assert_eq!(call(&mut p, nr::PSELECT6, [1, rd, 0, 0, tmo, 0]), 0);
    assert_eq!(fdset_get(&mut p, rd), 0);

    out.type_in(b"a\n");
    fdset(&mut p, rd, &[0]);
    fdset(&mut p, wr, &[1]);
    assert_eq!(call(&mut p, nr::PSELECT6, [2, rd, wr, 0, tmo, 0]), 2);
    assert_eq!(fdset_get(&mut p, rd), 1 << 0);
    assert_eq!(fdset_get(&mut p, wr), 1 << 1, "stdout is always writable");
}

#[test]
fn pselect_waits_until_the_timeout() {
    let (mut p, _) = process();
    let (rd, tmo) = (SCRATCH, SCRATCH + 0x20);
    // 1 ナノ秒だけ待つ:入力はないので 0 で戻る
    let ts: std::vec::Vec<u8> = [0u64, 1].iter().flat_map(|w| w.to_le_bytes()).collect();
    put(&mut p, tmo, &ts);
    fdset(&mut p, rd, &[0]);
    assert_eq!(call(&mut p, nr::PSELECT6, [1, rd, 0, 0, tmo, 0]), 0);
}

#[test]
fn ppoll_sets_revents() {
    let (mut p, out) = process();
    let fds = SCRATCH;
    // struct pollfd { fd: i32, events: i16, revents: i16 }
    let pollfd = |fd: i32, events: i16| {
        let mut b = std::vec::Vec::new();
        b.extend_from_slice(&fd.to_le_bytes());
        b.extend_from_slice(&events.to_le_bytes());
        b.extend_from_slice(&0i16.to_le_bytes());
        b
    };
    put(&mut p, fds, &[pollfd(0, 1), pollfd(1, 4)].concat()); // POLLIN, POLLOUT
    put(&mut p, SCRATCH + 0x40, &[0u8; 16]);
    assert_eq!(
        call(&mut p, nr::PPOLL, [fds, 2, SCRATCH + 0x40, 0, 8, 0]),
        1
    );
    assert_eq!(get(&mut p, fds + 6, 2), [0, 0]);
    assert_eq!(get(&mut p, fds + 14, 2), [4, 0]);
    out.type_in(b"z\n");
    assert_eq!(
        call(&mut p, nr::PPOLL, [fds, 2, SCRATCH + 0x40, 0, 8, 0]),
        2
    );
    assert_eq!(get(&mut p, fds + 6, 2), [1, 0]);
}
