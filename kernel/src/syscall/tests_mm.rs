//! メモリ関係とその他のシステムコールのテスト。

use super::tests::{SCRATCH, call, err, get, open, process, put};
use super::*;
use crate::Errno;
use hal::addr::VirtAddr;
use hal::mmu::MapFlags;

const PROT_READ: u64 = 1;
const PROT_WRITE: u64 = 2;
const MAP_PRIVATE: u64 = 0x02;
const MAP_FIXED: u64 = 0x10;
const MAP_ANONYMOUS: u64 = 0x20;
const NO_FD: u64 = -1i64 as u64;

#[test]
fn brk_grows_and_shrinks_the_heap() {
    let (mut p, _) = process();
    let start = call(&mut p, nr::BRK, [0; 6]);
    assert_eq!(start, 0x50_0000);
    assert_eq!(
        call(&mut p, nr::BRK, [start + 0x2345, 0, 0, 0, 0, 0]),
        start + 0x2345
    );
    p.mem
        .copy_to_user(VirtAddr(start + 0x2000), b"heap")
        .unwrap();
    assert_eq!(call(&mut p, nr::BRK, [start, 0, 0, 0, 0, 0]), start);
    assert_eq!(
        p.mem.copy_to_user(VirtAddr(start + 0x2000), b"x"),
        Err(Errno::EFAULT)
    );
    // 始まりより下には縮められない(今の値が返る)
    assert_eq!(
        call(&mut p, nr::BRK, [start - 0x1000, 0, 0, 0, 0, 0]),
        start
    );
}

#[test]
fn anonymous_mmap_returns_fresh_zeroed_writable_pages() {
    let (mut p, _) = process();
    let flags = MAP_PRIVATE | MAP_ANONYMOUS;
    let a = call(
        &mut p,
        nr::MMAP,
        [0, 0x3000, PROT_READ | PROT_WRITE, flags, NO_FD, 0],
    );
    let b = call(
        &mut p,
        nr::MMAP,
        [0, 0x1000, PROT_READ | PROT_WRITE, flags, NO_FD, 0],
    );
    assert_eq!(a % 4096, 0);
    assert!(b >= a + 0x3000 || b + 0x1000 <= a, "no overlap");
    assert_eq!(get(&mut p, a + 0x2ff8, 8), [0; 8]);
    put(&mut p, a + 0x2ff8, b"mmapped!");
}

#[test]
fn file_mmap_copies_the_contents() {
    let (mut p, _) = process();
    let fd = open(&mut p, b"/etc/motd", 0);
    let a = call(&mut p, nr::MMAP, [0, 100, PROT_READ, MAP_PRIVATE, fd, 0]);
    assert_eq!(get(&mut p, a, 18), b"Welcome to Pi-OS!\n");
    assert_eq!(
        get(&mut p, a + 18, 4),
        [0; 4],
        "the rest of the page is zero"
    );
}

#[test]
fn fixed_mmap_replaces_the_given_range() {
    let (mut p, _) = process();
    let flags = MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED;
    put(&mut p, SCRATCH, b"old");
    let a = call(
        &mut p,
        nr::MMAP,
        [SCRATCH, 0x1000, PROT_READ | PROT_WRITE, flags, NO_FD, 0],
    );
    assert_eq!(a, SCRATCH);
    assert_eq!(get(&mut p, SCRATCH, 3), [0; 3]);
}

#[test]
fn mmap_rejects_zero_length() {
    let (mut p, _) = process();
    let flags = MAP_PRIVATE | MAP_ANONYMOUS;
    assert_eq!(
        call(&mut p, nr::MMAP, [0, 0, PROT_READ, flags, NO_FD, 0]),
        err(Errno::EINVAL)
    );
}

#[test]
fn munmap_removes_pages_and_mprotect_changes_permissions() {
    let (mut p, _) = process();
    let flags = MAP_PRIVATE | MAP_ANONYMOUS;
    let a = call(
        &mut p,
        nr::MMAP,
        [0, 0x2000, PROT_READ | PROT_WRITE, flags, NO_FD, 0],
    );
    put(&mut p, a, b"keep");
    assert_eq!(
        call(&mut p, nr::MPROTECT, [a, 0x1000, PROT_READ, 0, 0, 0]),
        0
    );
    assert_eq!(p.mem.copy_to_user(VirtAddr(a), b"x"), Err(Errno::EFAULT));
    assert_eq!(get(&mut p, a, 4), b"keep", "contents survive mprotect");
    assert_eq!(
        call(&mut p, nr::MUNMAP, [a + 0x1000, 0x1000, 0, 0, 0, 0]),
        0
    );
    let mut b = [0u8; 1];
    assert_eq!(
        p.mem.copy_from_user(VirtAddr(a + 0x1000), &mut b),
        Err(Errno::EFAULT)
    );
    assert_eq!(call(&mut p, nr::MADVISE, [a, 0x1000, 4, 0, 0, 0]), 0);
}

#[test]
fn mremap_is_refused_so_libc_falls_back_to_copying() {
    let (mut p, _) = process();
    assert_eq!(
        call(&mut p, nr::MREMAP, [SCRATCH, 0x1000, 0x2000, 1, 0, 0]),
        err(Errno::ENOMEM)
    );
}

#[test]
fn identity_syscalls_describe_a_single_root_process() {
    let (mut p, _) = process();
    assert_eq!(
        call(&mut p, nr::SET_TID_ADDRESS, [SCRATCH, 0, 0, 0, 0, 0]),
        1
    );
    assert_eq!(call(&mut p, nr::GETPID, [0; 6]), 1);
    assert_eq!(call(&mut p, nr::GETTID, [0; 6]), 1);
    assert_eq!(call(&mut p, nr::GETUID, [0; 6]), 0);
    assert_eq!(call(&mut p, nr::GETEUID, [0; 6]), 0);
    assert_eq!(call(&mut p, nr::GETGID, [0; 6]), 0);
    assert_eq!(call(&mut p, nr::GETEGID, [0; 6]), 0);
}

#[test]
fn signal_setup_is_accepted_but_does_nothing_yet() {
    let (mut p, _) = process();
    assert_eq!(call(&mut p, nr::RT_SIGACTION, [2, SCRATCH, 0, 8, 0, 0]), 0);
    assert_eq!(
        call(
            &mut p,
            nr::RT_SIGPROCMASK,
            [0, SCRATCH, SCRATCH + 8, 8, 0, 0]
        ),
        0
    );
}

#[test]
fn sched_getaffinity_reports_one_cpu() {
    let (mut p, _) = process();
    assert_eq!(
        call(&mut p, nr::SCHED_GETAFFINITY, [0, 128, SCRATCH, 0, 0, 0]),
        8
    );
    assert_eq!(get(&mut p, SCRATCH, 8), [1, 0, 0, 0, 0, 0, 0, 0]);
}

#[test]
fn clock_gettime_writes_a_timespec() {
    let (mut p, _) = process();
    assert_eq!(call(&mut p, nr::CLOCK_GETTIME, [1, SCRATCH, 0, 0, 0, 0]), 0);
    let nsec = u64::from_le_bytes(get(&mut p, SCRATCH + 8, 8).try_into().unwrap());
    assert!(nsec < 1_000_000_000);
    assert_eq!(call(&mut p, nr::CLOCK_GETTIME, [0, SCRATCH, 0, 0, 0, 0]), 0);
    let sec = u64::from_le_bytes(get(&mut p, SCRATCH, 8).try_into().unwrap());
    assert!(
        sec >= crate::time::BOOT_EPOCH,
        "realtime starts at the build time"
    );
    assert_eq!(
        call(&mut p, nr::CLOCK_GETTIME, [99, SCRATCH, 0, 0, 0, 0]),
        err(Errno::EINVAL)
    );
}

#[test]
fn getrandom_fills_the_buffer() {
    let (mut p, _) = process();
    assert_eq!(call(&mut p, nr::GETRANDOM, [SCRATCH, 64, 0, 0, 0, 0]), 64);
    assert_ne!(get(&mut p, SCRATCH, 64), [0u8; 64]);
}

#[test]
fn uname_names_the_system() {
    let (mut p, _) = process();
    assert_eq!(call(&mut p, nr::UNAME, [SCRATCH, 0, 0, 0, 0, 0]), 0);
    assert_eq!(&get(&mut p, SCRATCH, 6), b"Pi-OS\0");
    assert_eq!(&get(&mut p, SCRATCH + 65 * 4, 8), b"aarch64\0");
}

#[test]
fn user_memory_protect_keeps_frames() {
    let (mut p, _) = process();
    p.mem.copy_to_user(VirtAddr(SCRATCH), b"data").unwrap();
    p.mem
        .protect(VirtAddr(SCRATCH), 0x1000, MapFlags::USER_R)
        .unwrap();
    assert_eq!(get(&mut p, SCRATCH, 4), b"data");
}

#[test]
fn table_numbers_are_sorted_and_unique_with_names() {
    for pair in TABLE.windows(2) {
        assert!(
            pair[0].nr < pair[1].nr,
            "{} / {}",
            pair[0].name,
            pair[1].name
        );
    }
    assert!(TABLE.iter().all(|s| !s.name.is_empty() && s.args <= 6));
    assert_eq!(lookup(nr::OPENAT).map(|s| s.name), Some("openat"));
    assert!(lookup(511).is_none());
}

#[test]
fn trace_line_shows_name_arguments_and_result() {
    let s = lookup(nr::OPENAT).unwrap();
    let line = trace_line(s, &[0xffff_ffff_ffff_ff9c, 0x40_3000, 0, 0, 0, 0], 3);
    assert_eq!(
        line,
        "[trace] openat(0xffffffffffffff9c, 0x403000, 0x0) = 3"
    );
    let s = lookup(nr::READ).unwrap();
    let line = trace_line(s, &[9, 0, 1, 0, 0, 0], Errno::EBADF.as_syscall_return());
    assert_eq!(line, "[trace] read(0x9, 0x0, 0x1) = -9 (EBADF)");
}

#[test]
fn readlinkat_on_files_without_links_is_einval_or_enoent() {
    let (mut p, _) = process();
    put(&mut p, SCRATCH + 0x200, b"/etc/motd\0");
    let at_fdcwd = -100i64 as u64;
    assert_eq!(
        call(
            &mut p,
            nr::READLINKAT,
            [at_fdcwd, SCRATCH + 0x200, SCRATCH, 64, 0, 0]
        ),
        err(Errno::EINVAL)
    );
    put(&mut p, SCRATCH + 0x200, b"/nope\0");
    assert_eq!(
        call(
            &mut p,
            nr::READLINKAT,
            [at_fdcwd, SCRATCH + 0x200, SCRATCH, 64, 0, 0]
        ),
        err(Errno::ENOENT)
    );
}

#[test]
fn identity_syscalls_report_the_process_credentials() {
    let (mut p, _) = process();
    p.cred = crate::cred::Credentials::user(1000, 100);
    p.cred.euid = 1001;
    p.cred.egid = 101;
    assert_eq!(call(&mut p, nr::GETUID, [0; 6]), 1000);
    assert_eq!(call(&mut p, nr::GETEUID, [0; 6]), 1001);
    assert_eq!(call(&mut p, nr::GETGID, [0; 6]), 100);
    assert_eq!(call(&mut p, nr::GETEGID, [0; 6]), 101);
}

#[test]
fn open_checks_permissions_against_the_process_credentials() {
    let (mut p, _) = process();
    // テスト用の initramfs の /etc/secret は root 所有の 0600
    p.cred = crate::cred::Credentials::user(1000, 100);
    assert_eq!(open(&mut p, b"/etc/secret", 0), err(Errno::EACCES));
    assert_eq!(
        open(&mut p, b"/etc/motd", 0),
        3,
        "0644 is readable by others"
    );
    p.cred = crate::cred::Credentials::root();
    assert_eq!(open(&mut p, b"/etc/secret", 0), 4);
}

#[test]
fn fstat_reports_the_owner() {
    let (mut p, _) = process();
    let fd = open(&mut p, b"/etc/secret", 0);
    assert_eq!(call(&mut p, nr::FSTAT, [fd, SCRATCH, 0, 0, 0, 0]), 0);
    let st = get(&mut p, SCRATCH, 128);
    assert_eq!(
        u32::from_le_bytes(st[24..28].try_into().unwrap()),
        0,
        "st_uid"
    );
    assert_eq!(
        u32::from_le_bytes(st[16..20].try_into().unwrap()) & 0o777,
        0o600
    );
}

const SCHED_OTHER: u64 = 0;
const SCHED_FIFO: u64 = 1;

#[test]
fn sched_setscheduler_makes_the_process_realtime() {
    let (mut p, _) = process();
    assert_eq!(call(&mut p, nr::SCHED_GETSCHEDULER, [0; 6]), SCHED_OTHER);
    put(&mut p, SCRATCH, &50i32.to_le_bytes());
    assert_eq!(
        call(
            &mut p,
            nr::SCHED_SETSCHEDULER,
            [0, SCHED_FIFO, SCRATCH, 0, 0, 0]
        ),
        0
    );
    assert_eq!(call(&mut p, nr::SCHED_GETSCHEDULER, [0; 6]), SCHED_FIFO);
    assert_eq!(p.rt_priority, 50);
    // 通常に戻すときは優先度 0
    put(&mut p, SCRATCH, &0i32.to_le_bytes());
    assert_eq!(
        call(
            &mut p,
            nr::SCHED_SETSCHEDULER,
            [0, SCHED_OTHER, SCRATCH, 0, 0, 0]
        ),
        0
    );
    assert_eq!(p.rt_priority, 0);
}

#[test]
fn sched_setscheduler_checks_its_arguments() {
    let (mut p, _) = process();
    put(&mut p, SCRATCH, &100i32.to_le_bytes());
    assert_eq!(
        call(
            &mut p,
            nr::SCHED_SETSCHEDULER,
            [0, SCHED_FIFO, SCRATCH, 0, 0, 0]
        ),
        err(Errno::EINVAL)
    );
    put(&mut p, SCRATCH, &10i32.to_le_bytes());
    assert_eq!(
        call(
            &mut p,
            nr::SCHED_SETSCHEDULER,
            [0, SCHED_OTHER, SCRATCH, 0, 0, 0]
        ),
        err(Errno::EINVAL)
    );
    assert_eq!(
        call(&mut p, nr::SCHED_SETSCHEDULER, [0, 7, SCRATCH, 0, 0, 0]),
        err(Errno::EINVAL)
    );
    // 他のプロセスはまだ対象にできない
    assert_eq!(
        call(
            &mut p,
            nr::SCHED_SETSCHEDULER,
            [999, SCHED_FIFO, SCRATCH, 0, 0, 0]
        ),
        err(Errno::ESRCH)
    );
    // リアルタイムにできるのは root だけ
    p.cred = crate::cred::Credentials::user(1000, 100);
    assert_eq!(
        call(
            &mut p,
            nr::SCHED_SETSCHEDULER,
            [0, SCHED_FIFO, SCRATCH, 0, 0, 0]
        ),
        err(Errno::EPERM)
    );
}

#[test]
fn sched_yield_returns_zero() {
    let (mut p, _) = process();
    assert_eq!(call(&mut p, nr::SCHED_YIELD, [0; 6]), 0);
}

#[test]
fn sigaltstack_is_accepted_and_reports_no_stack() {
    // Rust の標準ライブラリが起動時に呼ぶ(スタックあふれを知らせるシグナル用)。シグナルはまだ配送しない
    const SS_DISABLE: u32 = 2;
    let (mut p, _) = process();
    put(&mut p, SCRATCH + 0x100, &[0xff; 24]);
    assert_eq!(
        call(
            &mut p,
            nr::SIGALTSTACK,
            [SCRATCH, SCRATCH + 0x100, 0, 0, 0, 0]
        ),
        0
    );
    // stack_t { ss_sp, ss_flags, ss_size }
    let old = get(&mut p, SCRATCH + 0x100, 24);
    assert_eq!(old[..8], [0; 8]);
    assert_eq!(
        u32::from_le_bytes(old[8..12].try_into().unwrap()),
        SS_DISABLE
    );
    assert_eq!(call(&mut p, nr::SIGALTSTACK, [0, 0, 0, 0, 0, 0]), 0);
}

#[test]
fn sysinfo_reports_uptime_and_memory() {
    // struct sysinfo:uptime(8)、loads[3](24)、totalram(8)、freeram(8)、… mem_unit(4、104 バイト目)
    let (mut p, _) = process();
    put(&mut p, SCRATCH, &[0xff; 112]);
    assert_eq!(call(&mut p, nr::SYSINFO, [SCRATCH, 0, 0, 0, 0, 0]), 0);
    let b = get(&mut p, SCRATCH, 112);
    let word = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
    assert!(word(0) < 1_000_000, "uptime in seconds");
    // ホストのテストではページの割り当て器がないので 0。QEMU では実際の値
    assert!(word(40) <= word(32), "freeram <= totalram");
    assert_eq!(
        u32::from_le_bytes(b[104..108].try_into().unwrap()),
        1,
        "mem_unit"
    );
    assert_eq!(
        call(&mut p, nr::SYSINFO, [0, 0, 0, 0, 0, 0]),
        err(Errno::EFAULT)
    );
}
