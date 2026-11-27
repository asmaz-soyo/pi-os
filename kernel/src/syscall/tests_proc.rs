//! fork(clone)、execve、wait4、getppid、シバンのテスト。

use super::tests::{SCRATCH, call, err, get, process, put};
use super::*;
use crate::Errno;
use crate::process::{ExitStatus, PROCESSES};
use hal::addr::VirtAddr;

const SIGCHLD: u64 = 17;

fn fork(p: &mut crate::process::Process) -> crate::process::Process {
    match dispatch(p, nr::CLONE, [SIGCHLD, 0, 0, 0, 0, 0]) {
        Action::Fork {
            child, sp: None, ..
        } => *child,
        other => panic!("expected fork, got {other:?}"),
    }
}

#[test]
fn fork_copies_memory_so_parent_and_child_are_independent() {
    let (mut parent, _) = process();
    parent.pid = 100;
    put(&mut parent, SCRATCH, b"before");
    let mut child = fork(&mut parent);
    assert_eq!(child.ppid, 100);
    assert_eq!(get(&mut child, SCRATCH, 6), b"before");
    put(&mut child, SCRATCH, b"child!");
    assert_eq!(
        get(&mut parent, SCRATCH, 6),
        b"before",
        "parent is unchanged"
    );
}

#[test]
fn fork_shares_open_files() {
    let (mut parent, _) = process();
    let fd = super::tests::open(&mut parent, b"/etc/motd", 0);
    let mut child = fork(&mut parent);
    // 開いたファイル(位置を含む)は共有される
    assert_eq!(call(&mut child, nr::READ, [fd, SCRATCH, 8, 0, 0, 0]), 8);
    assert_eq!(call(&mut parent, nr::READ, [fd, SCRATCH, 2, 0, 0, 0]), 2);
    assert_eq!(get(&mut parent, SCRATCH, 2), b"to");
}

#[test]
fn getpid_and_gettid_return_the_process_number() {
    // 以前は決め打ちで 1 を返していた(シェルの $$ がいつも 1 になった)
    let (mut p, _) = process();
    p.pid = 42;
    p.tgid = 42; // スレッドが 1 つのプロセスは、グループの番号 = スレッドの番号
    assert_eq!(call(&mut p, nr::GETPID, [0; 6]), 42);
    assert_eq!(call(&mut p, nr::GETTID, [0; 6]), 42);
    assert_eq!(
        call(&mut p, nr::SET_TID_ADDRESS, [SCRATCH, 0, 0, 0, 0, 0]),
        42
    );
}

#[test]
fn getppid_returns_the_parent() {
    let (mut p, _) = process();
    p.ppid = 7;
    assert_eq!(call(&mut p, nr::GETPPID, [0; 6]), 7);
}

fn exec(p: &mut crate::process::Process, path: &[u8], argv: &[&[u8]]) -> Action {
    // 文字列とポインタの配列を作業領域に置く
    let mut at = SCRATCH + 0x800;
    let mut ptrs = std::vec::Vec::new();
    for a in argv {
        put(p, at, a);
        put(p, at + a.len() as u64, b"\0");
        ptrs.push(at);
        at += a.len() as u64 + 1;
    }
    ptrs.push(0);
    let argv_ptr = SCRATCH + 0x700;
    let bytes: std::vec::Vec<u8> = ptrs.iter().flat_map(|w| w.to_le_bytes()).collect();
    put(p, argv_ptr, &bytes);
    put(p, SCRATCH + 0x600, path);
    put(p, SCRATCH + 0x600 + path.len() as u64, b"\0");
    put(p, SCRATCH + 0x6f8, &0u64.to_le_bytes());
    dispatch(
        p,
        nr::EXECVE,
        [SCRATCH + 0x600, argv_ptr, SCRATCH + 0x6f8, 0, 0, 0],
    )
}

fn stack_strings(p: &mut crate::process::Process, sp: u64) -> std::vec::Vec<std::vec::Vec<u8>> {
    let argc = u64::from_le_bytes(get(p, sp, 8).try_into().unwrap());
    (0..argc)
        .map(|i| {
            let ptr = u64::from_le_bytes(get(p, sp + 8 + i * 8, 8).try_into().unwrap());
            p.mem.read_cstr(VirtAddr(ptr), 256).unwrap()
        })
        .collect()
}

#[test]
fn execve_replaces_the_program_and_memory() {
    let (mut p, _) = process();
    let Action::Exec { entry, sp } = exec(&mut p, b"/bin/tiny", &[b"tiny", b"arg"]) else {
        panic!("exec failed")
    };
    assert_eq!(entry, 0x40_1000);
    assert_eq!(stack_strings(&mut p, sp), [&b"tiny"[..], b"arg"]);
    // 前のプログラムの作業領域は消えている(tiny は 0x40_0000 にコードを置く)
    assert_eq!(get(&mut p, 0x40_0000, 4), b"CODE");
}

#[test]
fn execve_errors_leave_the_process_untouched() {
    let (mut p, _) = process();
    put(&mut p, SCRATCH + 0x100, b"keep");
    assert_eq!(
        exec(&mut p, b"/nope", &[b"nope"]),
        Action::Return(err(Errno::ENOENT))
    );
    assert_eq!(
        exec(&mut p, b"/etc/motd", &[b"motd"]),
        Action::Return(err(Errno::EACCES))
    );
    assert_eq!(
        exec(&mut p, b"/bin/x", &[b"x"]),
        Action::Return(err(Errno::ENOEXEC))
    );
    assert_eq!(get(&mut p, SCRATCH + 0x100, 4), b"keep");
}

#[test]
fn shebang_scripts_run_their_interpreter() {
    let (mut p, _) = process();
    let Action::Exec { sp, .. } = exec(&mut p, b"/bin/script", &[b"script", b"one"]) else {
        panic!("exec failed")
    };
    // #!/bin/tiny -x → argv = [/bin/tiny, -x, /bin/script, one]
    assert_eq!(
        stack_strings(&mut p, sp),
        [&b"/bin/tiny"[..], b"-x", b"/bin/script", b"one"]
    );
}

#[test]
fn wait4_collects_a_zombie_child_with_linux_status_encoding() {
    let (mut p, _) = process();
    p.pid = 200;
    PROCESSES.add_zombie(201, 200, ExitStatus::Code(3));
    let status = SCRATCH;
    assert_eq!(call(&mut p, nr::WAIT4, [u64::MAX, status, 0, 0, 0, 0]), 201);
    assert_eq!(
        get(&mut p, status, 4),
        (3u32 << 8).to_le_bytes(),
        "exited with 3"
    );
    PROCESSES.add_zombie(202, 200, ExitStatus::Signal(9));
    assert_eq!(call(&mut p, nr::WAIT4, [202, status, 0, 0, 0, 0]), 202);
    assert_eq!(
        get(&mut p, status, 4),
        9u32.to_le_bytes(),
        "killed by signal 9"
    );
}

#[test]
fn wait4_without_children_is_echild_and_wnohang_does_not_block() {
    let (mut p, _) = process();
    p.pid = 300;
    assert_eq!(
        call(&mut p, nr::WAIT4, [u64::MAX, 0, 0, 0, 0, 0]),
        err(Errno::ECHILD)
    );
    let (mut child, _) = process();
    child.ppid = 300;
    PROCESSES.insert(301, child);
    assert_eq!(
        call(&mut p, nr::WAIT4, [u64::MAX, 0, 1, 0, 0, 0]),
        0,
        "WNOHANG"
    );
    PROCESSES.exit(301, ExitStatus::Code(0));
    assert_eq!(call(&mut p, nr::WAIT4, [301, 0, 0, 0, 0, 0]), 301);
}

#[test]
fn chdir_changes_the_working_directory_for_relative_paths() {
    let (mut p, _) = process();
    put(&mut p, SCRATCH + 0x200, b"/etc\0");
    assert_eq!(call(&mut p, nr::CHDIR, [SCRATCH + 0x200, 0, 0, 0, 0, 0]), 0);
    assert_eq!(call(&mut p, nr::GETCWD, [SCRATCH, 64, 0, 0, 0, 0]), 5);
    assert_eq!(get(&mut p, SCRATCH, 5), b"/etc\0");
    assert_eq!(
        super::tests::open(&mut p, b"motd", 0),
        3,
        "relative to /etc"
    );
    put(&mut p, SCRATCH + 0x200, b"..\0");
    assert_eq!(call(&mut p, nr::CHDIR, [SCRATCH + 0x200, 0, 0, 0, 0, 0]), 0);
    assert_eq!(p.cwd, "/");
}

#[test]
fn chdir_errors() {
    let (mut p, _) = process();
    put(&mut p, SCRATCH + 0x200, b"/nope\0");
    assert_eq!(
        call(&mut p, nr::CHDIR, [SCRATCH + 0x200, 0, 0, 0, 0, 0]),
        err(Errno::ENOENT)
    );
    put(&mut p, SCRATCH + 0x200, b"/etc/motd\0");
    assert_eq!(
        call(&mut p, nr::CHDIR, [SCRATCH + 0x200, 0, 0, 0, 0, 0]),
        err(Errno::ENOTDIR)
    );
}

#[test]
fn execve_runs_programs_that_are_not_in_memory() {
    // tmpfs(や ext2)のファイルは中身を直接見せないので、読んでから読み込む
    let (mut p, _) = process();
    let tiny = p
        .vfs
        .lookup("/bin/tiny")
        .unwrap()
        .contents()
        .unwrap()
        .to_vec();
    p.vfs
        .create("/tmp/tiny", 0o755)
        .unwrap()
        .write(0, &tiny)
        .unwrap();
    let Action::Exec { entry, .. } = exec(&mut p, b"/tmp/tiny", &[b"tiny"]) else {
        panic!("exec from tmpfs failed")
    };
    assert_eq!(entry, 0x40_1000);
    assert_eq!(p.name, "tiny");
}

#[test]
fn prctl_set_name_renames_the_process() {
    const PR_SET_NAME: u64 = 15;
    let (mut p, _) = process();
    put(&mut p, SCRATCH, b"worker\0");
    assert_eq!(
        call(&mut p, nr::PRCTL, [PR_SET_NAME, SCRATCH, 0, 0, 0, 0]),
        0
    );
    assert_eq!(p.name, "worker");
    // ほかの操作はまだない
    assert_eq!(
        call(&mut p, nr::PRCTL, [9999, 0, 0, 0, 0, 0]),
        err(Errno::EINVAL)
    );
}

#[test]
fn clone_with_a_new_stack_starts_the_child_on_it() {
    // musl の posix_spawn(Rust の std::process::Command も使う)は
    // clone(CLONE_VM | CLONE_VFORK | SIGCHLD, 新しいスタック) で子を作り、子はそのスタックで動き出す
    const CLONE_VM: u64 = 0x100;
    const CLONE_VFORK: u64 = 0x4000;
    let (mut p, _) = process();
    match dispatch(
        &mut p,
        nr::CLONE,
        [CLONE_VM | CLONE_VFORK | SIGCHLD, 0x7000_0000, 0, 0, 0, 0],
    ) {
        Action::Fork { sp, .. } => assert_eq!(sp, Some(0x7000_0000)),
        other => panic!("expected fork, got {other:?}"),
    }
}

/// musl の pthread_create が clone に渡すフラグ
const THREAD_FLAGS: u64 = 0x100 // CLONE_VM
    | 0x200 // CLONE_FS
    | 0x400 // CLONE_FILES
    | 0x800 // CLONE_SIGHAND
    | 0x10000 // CLONE_THREAD
    | 0x40000 // CLONE_SYSVSEM
    | 0x80000 // CLONE_SETTLS
    | 0x100000 // CLONE_PARENT_SETTID
    | 0x200000 // CLONE_CHILD_CLEARTID
    | 0x400000; // CLONE_DETACHED

#[test]
fn clone_with_thread_flags_makes_a_thread_that_shares_memory_and_files() {
    let (mut p, _) = process();
    p.pid = 50;
    p.tgid = 50;
    // clone(flags, newsp, parent_tid, tls, child_tid)
    let action = dispatch(
        &mut p,
        nr::CLONE,
        [
            THREAD_FLAGS,
            0x7000_0000,
            SCRATCH + 0x10,
            0x7100_0000,
            SCRATCH + 0x20,
            0,
        ],
    );
    let Action::Fork {
        child,
        sp,
        tls,
        set_tid,
    } = action
    else {
        panic!("expected a new task, got {action:?}");
    };
    assert_eq!(sp, Some(0x7000_0000));
    assert_eq!(tls, Some(0x7100_0000), "CLONE_SETTLS");
    // 番号はまだ決まっていない。決まったら parent_tid に書く(同じメモリ)
    assert_eq!(set_tid, Some(SCRATCH + 0x10));
    assert_eq!(child.tgid, 50, "同じスレッドのグループ");
    assert_eq!(child.mem.id(), p.mem.id(), "CLONE_VM");
    assert_eq!(
        child.clear_child_tid,
        SCRATCH + 0x20,
        "CLONE_CHILD_CLEARTID"
    );
    // fd 表も共有(CLONE_FILES):親が開いたものが子に見える
    let fd = call(&mut p, nr::DUP, [1, 0, 0, 0, 0, 0]);
    assert!(child.fds.get(fd).is_ok());
}

#[test]
fn a_thread_without_shared_memory_is_rejected() {
    // CLONE_THREAD には CLONE_SIGHAND が、CLONE_SIGHAND には CLONE_VM が要る(Linux と同じ)
    let (mut p, _) = process();
    assert_eq!(
        call(&mut p, nr::CLONE, [0x10000 | 0x800, 0, 0, 0, 0, 0]),
        err(Errno::EINVAL)
    );
    assert_eq!(
        call(&mut p, nr::CLONE, [0x10000, 0, 0, 0, 0, 0]),
        err(Errno::EINVAL)
    );
}

#[test]
fn getpid_is_the_thread_group_and_gettid_the_thread() {
    let (mut p, _) = process();
    p.pid = 43;
    p.tgid = 42;
    assert_eq!(call(&mut p, nr::GETPID, [0; 6]), 42);
    assert_eq!(call(&mut p, nr::GETTID, [0; 6]), 43);
    // set_tid_address:終わるときに 0 を書いて futex で起こす場所。返すのはスレッドの番号
    assert_eq!(
        call(&mut p, nr::SET_TID_ADDRESS, [SCRATCH + 8, 0, 0, 0, 0, 0]),
        43
    );
    assert_eq!(p.clear_child_tid, SCRATCH + 8);
}

#[test]
fn exit_ends_the_thread_and_exit_group_the_whole_group() {
    let (mut p, _) = process();
    assert_eq!(
        dispatch(&mut p, nr::EXIT, [3, 0, 0, 0, 0, 0]),
        Action::Exit(3)
    );
    assert_eq!(
        dispatch(&mut p, nr::EXIT_GROUP, [4, 0, 0, 0, 0, 0]),
        Action::ExitGroup(4)
    );
}
