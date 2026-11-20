//! fork(clone)、execve、wait4、getppid、シバンのテスト。

use super::tests::{SCRATCH, call, err, get, process, put};
use super::*;
use crate::Errno;
use crate::process::{ExitStatus, PROCESSES};
use hal::addr::VirtAddr;

const SIGCHLD: u64 = 17;

fn fork(p: &mut crate::process::Process) -> crate::process::Process {
    match dispatch(p, nr::CLONE, [SIGCHLD, 0, 0, 0, 0, 0]) {
        Action::Fork(child) => *child,
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
