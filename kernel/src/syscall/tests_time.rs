//! nanosleep、timerfd、epoll のテスト。

use super::tests::{SCRATCH, call, err, get, process, put};
use super::*;
use crate::Errno;

fn timespec(sec: u64, nsec: u64) -> std::vec::Vec<u8> {
    [sec, nsec].iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// struct itimerspec { it_interval, it_value }
fn itimerspec(interval_ns: u64, value_ns: u64) -> std::vec::Vec<u8> {
    let mut v = timespec(interval_ns / 1_000_000_000, interval_ns % 1_000_000_000);
    v.extend(timespec(value_ns / 1_000_000_000, value_ns % 1_000_000_000));
    v
}

#[test]
fn nanosleep_waits_at_least_the_given_time() {
    let (mut p, _) = process();
    put(&mut p, SCRATCH, &timespec(0, 2_000_000));
    let before = crate::time::now_ns();
    assert_eq!(call(&mut p, nr::NANOSLEEP, [SCRATCH, 0, 0, 0, 0, 0]), 0);
    assert!(crate::time::now_ns() - before >= 2_000_000);
    assert_eq!(
        call(&mut p, nr::CLOCK_NANOSLEEP, [1, 0, SCRATCH, 0, 0, 0]),
        0
    );
}

#[test]
fn timerfd_counts_expirations_and_read_returns_them() {
    let (mut p, _) = process();
    let fd = call(&mut p, nr::TIMERFD_CREATE, [1, 0, 0, 0, 0, 0]);
    assert_eq!(fd, 3);
    // 1ms 後に最初、そのあと 1ms ごと
    put(&mut p, SCRATCH, &itimerspec(1_000_000, 1_000_000));
    assert_eq!(
        call(&mut p, nr::TIMERFD_SETTIME, [fd, 0, SCRATCH, 0, 0, 0]),
        0
    );
    assert_eq!(call(&mut p, nr::READ, [fd, SCRATCH + 0x100, 8, 0, 0, 0]), 8);
    let n = u64::from_le_bytes(get(&mut p, SCRATCH + 0x100, 8).try_into().unwrap());
    assert!(n >= 1, "at least one expiration, got {n}");
    // 読むと回数は 0 に戻る。gettime は次の満了までの残り時間を返す
    assert_eq!(
        call(
            &mut p,
            nr::TIMERFD_GETTIME,
            [fd, SCRATCH + 0x200, 0, 0, 0, 0]
        ),
        0
    );
    let interval = u64::from_le_bytes(get(&mut p, SCRATCH + 0x208, 8).try_into().unwrap());
    assert_eq!(interval, 1_000_000);
}

#[test]
fn reading_a_timerfd_needs_an_8_byte_buffer() {
    let (mut p, _) = process();
    let fd = call(&mut p, nr::TIMERFD_CREATE, [1, 0, 0, 0, 0, 0]);
    assert_eq!(
        call(&mut p, nr::READ, [fd, SCRATCH, 4, 0, 0, 0]),
        err(Errno::EINVAL)
    );
}

const EPOLLIN: u32 = 1;
const EPOLL_CTL_ADD: u64 = 1;
const EPOLL_CTL_DEL: u64 = 2;

/// struct epoll_event(AArch64 では 16 バイト:events、詰め物、data)
fn event(events: u32, data: u64) -> std::vec::Vec<u8> {
    let mut v = events.to_le_bytes().to_vec();
    v.extend([0u8; 4]);
    v.extend(data.to_le_bytes());
    v
}

#[test]
fn epoll_reports_ready_file_descriptors_with_their_data() {
    let (mut p, _) = process();
    let ep = call(&mut p, nr::EPOLL_CREATE1, [0, 0, 0, 0, 0, 0]);
    let tfd = call(&mut p, nr::TIMERFD_CREATE, [1, 0, 0, 0, 0, 0]);
    put(&mut p, SCRATCH, &event(EPOLLIN, 0xabcd));
    assert_eq!(
        call(
            &mut p,
            nr::EPOLL_CTL,
            [ep, EPOLL_CTL_ADD, tfd, SCRATCH, 0, 0]
        ),
        0
    );
    // まだタイマを動かしていないので、待たずに 0
    assert_eq!(
        call(&mut p, nr::EPOLL_PWAIT, [ep, SCRATCH + 0x100, 4, 0, 0, 0]),
        0
    );
    put(&mut p, SCRATCH + 0x200, &itimerspec(0, 1_000_000));
    call(
        &mut p,
        nr::TIMERFD_SETTIME,
        [tfd, 0, SCRATCH + 0x200, 0, 0, 0],
    );
    // 無期限に待つ(-1)と、1ms 後に準備ができる
    assert_eq!(
        call(
            &mut p,
            nr::EPOLL_PWAIT,
            [ep, SCRATCH + 0x100, 4, u64::MAX, 0, 0]
        ),
        1
    );
    assert_eq!(get(&mut p, SCRATCH + 0x100, 16), event(EPOLLIN, 0xabcd));
}

#[test]
fn epoll_ctl_errors_and_removal() {
    let (mut p, _) = process();
    let ep = call(&mut p, nr::EPOLL_CREATE1, [0, 0, 0, 0, 0, 0]);
    put(&mut p, SCRATCH, &event(EPOLLIN, 1));
    assert_eq!(
        call(
            &mut p,
            nr::EPOLL_CTL,
            [ep, EPOLL_CTL_ADD, 77, SCRATCH, 0, 0]
        ),
        err(Errno::EBADF)
    );
    assert_eq!(
        call(&mut p, nr::EPOLL_CTL, [ep, EPOLL_CTL_ADD, 1, SCRATCH, 0, 0]),
        0
    );
    assert_eq!(
        call(&mut p, nr::EPOLL_CTL, [ep, EPOLL_CTL_ADD, 1, SCRATCH, 0, 0]),
        err(Errno::EEXIST)
    );
    assert_eq!(
        call(&mut p, nr::EPOLL_CTL, [ep, EPOLL_CTL_DEL, 1, 0, 0, 0]),
        0
    );
    assert_eq!(
        call(&mut p, nr::EPOLL_CTL, [ep, EPOLL_CTL_DEL, 1, 0, 0, 0]),
        err(Errno::ENOENT)
    );
    assert_eq!(
        call(&mut p, nr::EPOLL_CTL, [1, EPOLL_CTL_ADD, 2, SCRATCH, 0, 0]),
        err(Errno::EINVAL),
        "not an epoll fd"
    );
}
