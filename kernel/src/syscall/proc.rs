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
const CLONE_VFORK: u64 = 0x4000;

/// clone(flags, newsp, …)。今は fork と vfork の形だけ(スレッドはまだ)。
/// `newsp` が 0 でなければ、子はそのスタックで動き出す。
pub fn clone(p: &mut Process, flags: u64, newsp: u64) -> Action {
    // vfork(CLONE_VM | CLONE_VFORK)も、メモリを写す fork として扱う。
    // vfork の子は exec か _exit しかしない約束なので、写しても結果は同じ
    if flags & !(CSIGNAL | CLONE_VM | CLONE_VFORK) != 0 {
        return Action::Return(Errno::EINVAL.as_syscall_return());
    }
    match p.fork() {
        Ok(child) => Action::Fork {
            child: Box::new(child),
            sp: (newsp != 0).then_some(newsp),
        },
        Err(e) => Action::Return(e.as_syscall_return()),
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
        let bytes = crate::exec::read_image(&*file)?;
        match shebang(&bytes) {
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
                image = Some(bytes);
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
    let loaded = crate::exec::load(&mut mem, &image, &argv_refs, &envp_refs, random)?;
    p.mem = mem;
    p.fds.close_on_exec();
    p.name = String::from(path.rsplit('/').next().unwrap_or(&path));
    PROCESSES.rename(p.pid, &p.name);
    p.entry = loaded;
    p.brk = loaded.brk;
    p.mmap_next = MMAP_BASE;
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
    loop {
        if let Some((child, st)) = PROCESSES.wait_child(p.pid, filter)? {
            if status != 0 {
                p.mem
                    .copy_to_user(VirtAddr(status), &st.wait_status().to_le_bytes())?;
            }
            return Ok(child as u64);
        }
        if options & WNOHANG != 0 {
            return Ok(0);
        }
        crate::sched::relax();
    }
}
