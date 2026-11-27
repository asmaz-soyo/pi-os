//! プロセス関係のシステムコール(exit、clone、execve、wait4)。

use super::Action;
use crate::Errno;
use crate::cred::Access;
use crate::mm::user::UserMemory;
use crate::process::{MMAP_BASE, PROCESSES, Process};
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use hal::addr::VirtAddr;

/// exit / exit_group の引数を終了ステータスにする(下位 8 ビットだけが親に見える)。
pub fn exit_status(arg: u64) -> i32 {
    (arg & 0xff) as i32
}

const CSIGNAL: u64 = 0xff;
const CLONE_VM: u64 = 0x100;
const CLONE_FS: u64 = 0x200;
const CLONE_FILES: u64 = 0x400;
const CLONE_SIGHAND: u64 = 0x800;
const CLONE_VFORK: u64 = 0x4000;
const CLONE_THREAD: u64 = 0x10000;
const CLONE_SYSVSEM: u64 = 0x40000;
const CLONE_SETTLS: u64 = 0x80000;
const CLONE_PARENT_SETTID: u64 = 0x100000;
const CLONE_CHILD_CLEARTID: u64 = 0x200000;
const CLONE_DETACHED: u64 = 0x400000;
const CLONE_CHILD_SETTID: u64 = 0x1000000;
/// スレッドを作るときに受け付けるフラグ(CLONE_FS と CLONE_SYSVSEM は受け付けるだけで、まだ共有しない)
const THREAD_ALLOWED: u64 = CLONE_VM
    | CLONE_FS
    | CLONE_FILES
    | CLONE_SIGHAND
    | CLONE_THREAD
    | CLONE_SYSVSEM
    | CLONE_SETTLS
    | CLONE_PARENT_SETTID
    | CLONE_CHILD_CLEARTID
    | CLONE_DETACHED
    | CLONE_CHILD_SETTID;

/// clone(flags, newsp, …)。今は fork と vfork の形だけ(スレッドはまだ)。
/// `newsp` が 0 でなければ、子はそのスタックで動き出す。
/// clone(flags, newsp, parent_tid, tls, child_tid)(aarch64 の引数の順)
///
/// - CLONE_THREAD があればスレッド(メモリと fd の表を共有する。musl の pthread_create)
/// - なければ fork。vfork(CLONE_VM | CLONE_VFORK)も、メモリを写す fork として扱う
///   (vfork の子は exec か _exit しかしない約束なので、写しても結果は同じ)
pub fn clone(p: &mut Process, flags: u64, newsp: u64, ptid: u64, tls: u64, ctid: u64) -> Action {
    let fail = |e: Errno| Action::Return(e.as_syscall_return());
    let sp = (newsp != 0).then_some(newsp);
    if flags & CLONE_THREAD != 0 {
        // スレッドはシグナルの扱いを、シグナルの扱いはメモリを共有する(Linux と同じ決まり)
        let needs = CLONE_VM | CLONE_SIGHAND | CLONE_FILES;
        if flags & needs != needs || flags & !THREAD_ALLOWED != 0 {
            return fail(Errno::EINVAL);
        }
        let mut child = p.thread();
        if flags & CLONE_CHILD_CLEARTID != 0 {
            child.clear_child_tid = ctid;
        }
        let set_tid = if flags & CLONE_PARENT_SETTID != 0 {
            Some(ptid)
        } else {
            (flags & CLONE_CHILD_SETTID != 0).then_some(ctid)
        };
        return Action::Fork {
            child: Box::new(child),
            sp,
            tls: (flags & CLONE_SETTLS != 0).then_some(tls),
            set_tid,
        };
    }
    if flags & !(CSIGNAL | CLONE_VM | CLONE_VFORK) != 0 {
        return fail(Errno::EINVAL);
    }
    match p.fork() {
        Ok(child) => Action::Fork {
            child: Box::new(child),
            sp,
            tls: None,
            set_tid: None,
        },
        Err(e) => fail(e),
    }
}

/// NULL で終わるポインタの配列が指す文字列を読む(argv、envp)。
fn string_array(p: &mut Process, ptr: u64) -> Result<Vec<Vec<u8>>, Errno> {
    let mut out = Vec::new();
    if ptr == 0 {
        return Ok(out);
    }
    for i in 0..1024u64 {
        let mut b = [0u8; 8];
        p.mem.copy_from_user(VirtAddr(ptr + i * 8), &mut b)?;
        let s = u64::from_le_bytes(b);
        if s == 0 {
            return Ok(out);
        }
        out.push(p.mem.read_cstr(VirtAddr(s), 4096)?);
    }
    Err(Errno::E2BIG)
}

/// シバン(`#!インタプリタ 引数`)の行を読む。
fn shebang(image: &[u8]) -> Option<(String, Option<String>)> {
    let rest = image.strip_prefix(b"#!")?;
    let line = rest.split(|&b| b == b'\n').next()?;
    let line = core::str::from_utf8(line).ok()?.trim();
    let mut parts = line.splitn(2, char::is_whitespace);
    let interp = String::from(parts.next()?);
    let arg = parts
        .next()
        .map(|a| String::from(a.trim()))
        .filter(|a| !a.is_empty());
    Some((interp, arg))
}

fn exec_inner(p: &mut Process, path: u64, argv: u64, envp: u64) -> Result<(u64, u64), Errno> {
    let raw = p.mem.read_cstr(VirtAddr(path), 4096)?;
    let mut path = crate::fs::vfs::normalize(
        &p.cwd,
        core::str::from_utf8(&raw).map_err(|_| Errno::EINVAL)?,
    );
    let mut argv = string_array(p, argv)?;
    let envp = string_array(p, envp)?;
    // シバンのスクリプトなら、インタプリタを実行する(入れ子は 1 段まで)
    let mut image = None;
    for _ in 0..2 {
        let file = p.vfs.lookup(&path)?;
        p.cred.check(&file.stat(), Access::EXEC)?;
        if file.stat().kind != crate::fs::vfs::FileKind::Regular {
            return Err(Errno::EACCES);
        }
        // 先頭の 1 行だけを見る(シバンの行は 256 バイトまで)
        let mut head = [0u8; 256];
        let n = file.read(0, &mut head)?;
        match shebang(&head[..n]) {
            Some((interp, arg)) => {
                let mut new_argv = alloc::vec![interp.clone().into_bytes()];
                if let Some(arg) = arg {
                    new_argv.push(arg.into_bytes());
                }
                new_argv.push(path.clone().into_bytes());
                new_argv.extend(argv.into_iter().skip(1));
                argv = new_argv;
                path = interp;
            }
            None => {
                image = Some(file);
                break;
            }
        }
    }
    let image = image.ok_or(Errno::ELOOP)?;
    // 新しいユーザー空間に読み込む。失敗しても今のプロセスはそのまま
    let mut mem = UserMemory::new((p.new_space)()?, p.mem.phys());
    let argv_refs: Vec<&[u8]> = argv.iter().map(Vec::as_slice).collect();
    let envp_refs: Vec<&[u8]> = envp.iter().map(Vec::as_slice).collect();
    let mut random = [0u8; 16];
    super::misc::fill_random(&mut random);
    // 動的リンクなら、動的リンカ(PT_INTERP のパス)も読み込む
    let interp = match crate::exec::interpreter(&*image)? {
        Some(path) => Some(p.vfs.lookup(&path)?),
        None => None,
    };
    // ファイルから少しずつ読んで読み込む(全体をカーネルのヒープに置かない)
    let loaded = crate::exec::load_with_interp(
        &mut mem,
        &*image,
        interp.as_deref(),
        &argv_refs,
        &envp_refs,
        random,
    )?;
    p.mem = mem;
    p.fds.close_on_exec();
    // ハンドラは新しいプログラムにないので既定に戻す(ほかのスレッドとは、もう共有しない)
    let mut actions = p.sig_actions.lock().clone();
    actions.reset_for_exec();
    p.sig_actions = alloc::sync::Arc::new(crate::sync::SpinLock::new(actions));
    p.name = String::from(path.rsplit('/').next().unwrap_or(&path));
    PROCESSES.rename(p.pid, &p.name);
    p.entry = loaded;
    p.mem.set_brk(loaded.brk);
    p.mem.set_mmap_next(MMAP_BASE);
    Ok((loaded.entry, loaded.sp))
}

/// execve(path, argv, envp)
pub fn execve(p: &mut Process, path: u64, argv: u64, envp: u64) -> Action {
    match exec_inner(p, path, argv, envp) {
        Ok((entry, sp)) => Action::Exec { entry, sp },
        Err(e) => Action::Return(e.as_syscall_return()),
    }
}

const WNOHANG: u64 = 1;

/// wait4(pid, status, options, rusage)
pub fn wait4(p: &mut Process, pid: i64, status: u64, options: u64) -> Result<u64, Errno> {
    let filter = if pid > 0 { Some(pid as u32) } else { None };
    let found = if options & WNOHANG != 0 {
        PROCESSES.wait_child(p.pid, filter)?
    } else {
        // 子が終わるのを眠って待つ(終わった子があれば、すぐ返る)
        let me = p.pid;
        let r = crate::wait::wait_event(&[&crate::process::CHILD_WQ], None, || {
            PROCESSES.wait_child(me, filter).transpose()
        });
        r.transpose()?
    };
    let Some((child, st)) = found else {
        return Ok(0);
    };
    if status != 0 {
        p.mem
            .copy_to_user(VirtAddr(status), &st.wait_status().to_le_bytes())?;
    }
    Ok(child as u64)
}
