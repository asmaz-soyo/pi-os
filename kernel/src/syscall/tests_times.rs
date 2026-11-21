//! ファイルの時刻(stat の atime / mtime / ctime、utimensat)のテスト。tmpfs(/tmp)で。

use super::tests::{SCRATCH, call, err, get, open, process, put};
use super::*;
use crate::Errno;
use crate::process::Process;

const AT_FDCWD: u64 = -100i64 as u64;
const O_WRONLY: u64 = 0o1;
const O_CREAT: u64 = 0o100;
const UTIME_NOW: u64 = 0x3fff_ffff;
const UTIME_OMIT: u64 = 0x3fff_fffe;

fn cstr(p: &mut Process, s: &[u8]) -> u64 {
    put(p, SCRATCH + 0x3800, &[s, b"\0"].concat());
    SCRATCH + 0x3800
}

/// (atime, mtime, ctime) の秒
fn times(p: &mut Process, path: &[u8]) -> (i64, i64, i64) {
    let at = cstr(p, path);
    assert_eq!(
        call(p, nr::NEWFSTATAT, [AT_FDCWD, at, SCRATCH + 0x400, 0, 0, 0]),
        0
    );
    let b = get(p, SCRATCH + 0x400, 128);
    let w = |i: usize| i64::from_le_bytes(b[i..i + 8].try_into().unwrap());
    (w(72), w(88), w(104))
}

/// utimensat(AT_FDCWD, path, [(atime 秒, ナノ秒), (mtime 秒, ナノ秒)])
fn utimens(p: &mut Process, path: &[u8], ts: Option<[(u64, u64); 2]>) -> u64 {
    let at = cstr(p, path);
    let ptr = match ts {
        Some(t) => {
            let mut b = std::vec::Vec::new();
            for (s, ns) in t {
                b.extend_from_slice(&s.to_le_bytes());
                b.extend_from_slice(&ns.to_le_bytes());
            }
            put(p, SCRATCH + 0x600, &b);
            SCRATCH + 0x600
        }
        None => 0,
    };
    call(p, nr::UTIMENSAT, [AT_FDCWD, at, ptr, 0, 0, 0])
}

fn now() -> i64 {
    (crate::time::BOOT_EPOCH + crate::time::now_ns() / 1_000_000_000) as i64
}

#[test]
fn new_files_carry_the_current_time() {
    let (mut p, _) = process();
    open(&mut p, b"/tmp/f", O_CREAT | O_WRONLY);
    let (a, m, c) = times(&mut p, b"/tmp/f");
    let t = now();
    for x in [a, m, c] {
        assert!((t - 2..=t).contains(&x), "{x} vs now {t}");
    }
}

#[test]
fn utimensat_sets_access_and_modification_times() {
    let (mut p, _) = process();
    open(&mut p, b"/tmp/f", O_CREAT | O_WRONLY);
    assert_eq!(utimens(&mut p, b"/tmp/f", Some([(1000, 0), (2000, 5)])), 0);
    let (a, m, _) = times(&mut p, b"/tmp/f");
    assert_eq!((a, m), (1000, 2000));
    // UTIME_OMIT はそのまま、UTIME_NOW は今
    assert_eq!(
        utimens(&mut p, b"/tmp/f", Some([(0, UTIME_OMIT), (0, UTIME_NOW)])),
        0
    );
    let (a, m, _) = times(&mut p, b"/tmp/f");
    assert_eq!(a, 1000);
    assert!(m >= now() - 2);
    // NULL なら両方とも今
    assert_eq!(utimens(&mut p, b"/tmp/f", None), 0);
    assert!(times(&mut p, b"/tmp/f").0 >= now() - 2);
    assert_eq!(utimens(&mut p, b"/tmp/nope", None), err(Errno::ENOENT));
}

#[test]
fn writing_updates_mtime_and_chmod_updates_ctime() {
    let (mut p, _) = process();
    let fd = open(&mut p, b"/tmp/f", O_CREAT | O_WRONLY);
    utimens(&mut p, b"/tmp/f", Some([(1000, 0), (1000, 0)]));
    put(&mut p, SCRATCH, b"data");
    call(&mut p, nr::WRITE, [fd, SCRATCH, 4, 0, 0, 0]);
    let (a, m, _) = times(&mut p, b"/tmp/f");
    assert_eq!(a, 1000, "reading or writing does not touch atime here");
    assert!(m >= now() - 2);
    // chmod は中身を変えないので mtime はそのまま、ctime(属性の変更)は今
    utimens(&mut p, b"/tmp/f", Some([(1000, 0), (1000, 0)]));
    let at = cstr(&mut p, b"/tmp/f");
    call(&mut p, nr::FCHMODAT, [AT_FDCWD, at, 0o600, 0, 0, 0]);
    let (_, m, c) = times(&mut p, b"/tmp/f");
    assert_eq!(m, 1000);
    assert!(c >= now() - 2);
}

#[test]
fn only_the_owner_may_set_explicit_times() {
    let (mut p, _) = process();
    open(&mut p, b"/tmp/roots", O_CREAT | O_WRONLY);
    p.cred = crate::cred::Credentials::user(1000, 100);
    assert_eq!(
        utimens(&mut p, b"/tmp/roots", Some([(1, 0), (1, 0)])),
        err(Errno::EPERM)
    );
}
