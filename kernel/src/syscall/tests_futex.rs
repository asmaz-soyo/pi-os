//! futex のテスト(眠って起こされるところは QEMU の pthread のテストで確かめる)。

use super::tests::{SCRATCH, call, err, process, put};
use super::*;
use crate::Errno;

const FUTEX_WAIT: u64 = 0;
const FUTEX_WAKE: u64 = 1;
const FUTEX_PRIVATE: u64 = 128;

fn timespec(sec: u64, nsec: u64) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[..8].copy_from_slice(&sec.to_le_bytes());
    b[8..].copy_from_slice(&nsec.to_le_bytes());
    b
}

#[test]
fn wait_returns_eagain_when_the_value_has_already_changed() {
    let (mut p, _) = process();
    put(&mut p, SCRATCH, &5u32.to_le_bytes());
    // 「まだ 4 なら眠らせて」:もう 5 なので、すぐ戻る(ほかのスレッドが先に進めた)
    assert_eq!(
        call(
            &mut p,
            nr::FUTEX,
            [SCRATCH, FUTEX_WAIT | FUTEX_PRIVATE, 4, 0, 0, 0]
        ),
        err(Errno::EAGAIN)
    );
}

#[test]
fn wait_with_a_timeout_gives_up_with_etimedout() {
    let (mut p, _) = process();
    put(&mut p, SCRATCH, &4u32.to_le_bytes());
    put(&mut p, SCRATCH + 0x40, &timespec(0, 1_000_000)); // 1ms(相対)
    assert_eq!(
        call(
            &mut p,
            nr::FUTEX,
            [SCRATCH, FUTEX_WAIT, 4, SCRATCH + 0x40, 0, 0]
        ),
        err(Errno::ETIMEDOUT)
    );
    // 期限が切れたら列から外れている(あとの WAKE は、だれも起こさない)
    assert_eq!(
        call(&mut p, nr::FUTEX, [SCRATCH, FUTEX_WAKE, 1, 0, 0, 0]),
        0
    );
}

#[test]
fn wake_wakes_queued_waiters_and_counts_them() {
    let (mut p, _) = process();
    assert_eq!(
        call(
            &mut p,
            nr::FUTEX,
            [SCRATCH, FUTEX_WAKE | FUTEX_PRIVATE, 1, 0, 0, 0]
        ),
        0
    );
    // 待っているスレッドのふり(番号 7001、7002 が同じ鍵で列にいる)
    let key = (p.mem.id(), SCRATCH);
    crate::futex::FUTEXES.lock().enqueue(key, 7001);
    crate::futex::FUTEXES.lock().enqueue(key, 7002);
    assert_eq!(
        call(&mut p, nr::FUTEX, [SCRATCH, FUTEX_WAKE, 1, 0, 0, 0]),
        1
    );
    assert!(!crate::futex::FUTEXES.lock().is_queued(key, 7001));
    assert_eq!(
        call(
            &mut p,
            nr::FUTEX,
            [SCRATCH, FUTEX_WAKE, i32::MAX as u64, 0, 0, 0]
        ),
        1
    );
}

#[test]
fn bad_addresses_and_unknown_operations_are_rejected() {
    let (mut p, _) = process();
    assert_eq!(
        call(&mut p, nr::FUTEX, [SCRATCH + 1, FUTEX_WAIT, 0, 0, 0, 0]),
        err(Errno::EINVAL),
        "4 バイトの境界にない"
    );
    assert_eq!(
        call(&mut p, nr::FUTEX, [0x10, FUTEX_WAIT, 0, 0, 0, 0]),
        err(Errno::EFAULT)
    );
    assert_eq!(
        call(&mut p, nr::FUTEX, [SCRATCH, 77, 0, 0, 0, 0]),
        err(Errno::ENOSYS)
    );
}
