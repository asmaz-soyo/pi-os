//! ファイル関係のシステムコール。

use super::TIOCGWINSZ;
use crate::tty::{TERMIOS_SIZE, Termios};

// ioctl の番号(端末の設定)
const TCGETS: u64 = 0x5401;
const TCSETS: u64 = 0x5402;
const TCSETSW: u64 = 0x5403;
const TCSETSF: u64 = 0x5404;
use crate::Errno;
use crate::cred::Access;
use crate::fs::vfs::{
    self, FileKind, O_ACCMODE, O_CREAT, O_DIRECTORY, O_EXCL, O_RDONLY, O_TRUNC, O_WRONLY, OpenFile,
    Stat,
};
use crate::process::Process;
use alloc::sync::Arc;
use alloc::vec;
use hal::addr::VirtAddr;

/// 一度にユーザーとやり取りする量の上限
const CHUNK: usize = 64 * 1024;
/// パスの長さの上限(Linux の PATH_MAX)
const PATH_MAX: usize = 4096;
/// openat などの dirfd で「今のディレクトリ」を表す値
const AT_FDCWD: i64 = -100;
const AT_EMPTY_PATH: u64 = 0x1000;

fn path_arg(p: &mut Process, dirfd: u64, ptr: u64) -> Result<alloc::string::String, Errno> {
    let raw = p.mem.read_cstr(VirtAddr(ptr), PATH_MAX)?;
    let path = core::str::from_utf8(&raw).map_err(|_| Errno::EINVAL)?;
    // dirfd は int:上位 32 ビットは見ない(int の -100 が 0x0000_0000_ffff_ff9c で届くことがある)
    if path.starts_with('/') || dirfd as i32 as i64 == AT_FDCWD {
        return Ok(vfs::normalize(&p.cwd, path));
    }
    // 相対パスは、ディレクトリの fd を開いたときのパスを基準にする
    let dir = p.fds.get(dirfd as u32 as u64)?;
    if dir.file.stat().kind != FileKind::Directory {
        return Err(Errno::ENOTDIR);
    }
    let base = dir.path.clone().ok_or(Errno::ENOTDIR)?;
    Ok(vfs::normalize(&base, path))
}

/// openat(dirfd, path, flags, mode)
pub fn openat(p: &mut Process, dirfd: u64, path: u64, flags: u64, mode: u64) -> Result<u64, Errno> {
    let flags = flags as u32;
    let path = path_arg(p, dirfd, path)?;
    // O_NOFOLLOW:最後の要素がシンボリックリンクなら開かない(AArch64 の値。x86 は 0o400000)
    const O_NOFOLLOW: u32 = 0o100000;
    if flags & O_NOFOLLOW != 0
        && p.vfs
            .lookup_nofollow(&path)
            .is_ok_and(|f| f.stat().kind == FileKind::Symlink)
    {
        return Err(Errno::ELOOP);
    }
    let file = match p.vfs.lookup(&path) {
        Ok(_) if flags & O_CREAT != 0 && flags & O_EXCL != 0 => return Err(Errno::EEXIST),
        Ok(file) => file,
        Err(Errno::ENOENT) if flags & O_CREAT != 0 => {
            // 作るには、親ディレクトリに書き込みと実行(たどる)の権限が要る
            let parent = vfs::normalize(&path, "..");
            if let Ok(dir) = p.vfs.lookup(&parent) {
                p.cred.check(&dir.stat(), Access::WRITE | Access::EXEC)?;
            }
            let file = p.vfs.create(&path, mode as u32 & 0o777)?;
            own_new(p, &*file)?;
            file
        }
        Err(e) => return Err(e),
    };
    let st = file.stat();
    if flags & O_DIRECTORY != 0 && st.kind != FileKind::Directory {
        return Err(Errno::ENOTDIR);
    }
    let wants_write = flags & O_ACCMODE != O_RDONLY || flags & O_TRUNC != 0;
    if wants_write {
        if st.kind == FileKind::Directory {
            return Err(Errno::EISDIR);
        }
        if file.is_read_only() {
            return Err(Errno::EROFS);
        }
    }
    let wants_read = flags & O_ACCMODE != O_WRONLY;
    let mut want = Access::NONE;
    if wants_read && st.kind != FileKind::Directory {
        want = want | Access::READ;
    }
    if wants_write {
        want = want | Access::WRITE;
    }
    p.cred.check(&st, want)?;
    if flags & O_TRUNC != 0 && st.kind == FileKind::Regular {
        file.truncate(0)?;
    }
    let fd = p.fds.insert(OpenFile::with_path(file, flags, path))?;
    Ok(fd as u64)
}

const AT_REMOVEDIR: u64 = 0x200;

/// mkdirat(dirfd, path, mode)
pub fn mkdirat(p: &mut Process, dirfd: u64, path: u64, mode: u64) -> Result<u64, Errno> {
    let path = path_arg(p, dirfd, path)?;
    p.vfs.mkdir(&path, mode as u32 & 0o7777)?;
    own_new(p, &*p.vfs.lookup_nofollow(&path)?)?;
    Ok(0)
}

/// unlinkat(dirfd, path, flags)
pub fn unlinkat(p: &mut Process, dirfd: u64, path: u64, flags: u64) -> Result<u64, Errno> {
    let path = path_arg(p, dirfd, path)?;
    p.vfs.unlink(&path, flags & AT_REMOVEDIR != 0)?;
    Ok(0)
}

/// renameat2(olddirfd, old, newdirfd, new, flags)。flags は RENAME_NOREPLACE だけ。
/// 両方の親ディレクトリに、書き込みとたどる(実行)権限が要る。
pub fn renameat2(
    p: &mut Process,
    olddirfd: u64,
    old: u64,
    newdirfd: u64,
    new: u64,
    flags: u64,
) -> Result<u64, Errno> {
    const RENAME_NOREPLACE: u64 = 1;
    if flags & !RENAME_NOREPLACE != 0 {
        return Err(Errno::EINVAL);
    }
    let old = path_arg(p, olddirfd, old)?;
    let new = path_arg(p, newdirfd, new)?;
    for path in [&old, &new] {
        let dir = p.vfs.lookup(&vfs::normalize(path, ".."))?;
        p.cred.check(&dir.stat(), Access::WRITE | Access::EXEC)?;
    }
    p.vfs.rename(&old, &new, flags & RENAME_NOREPLACE != 0)?;
    Ok(0)
}

/// ftruncate(fd, length)
pub fn ftruncate(p: &mut Process, fd: u64, len: u64) -> Result<u64, Errno> {
    let f = p.fds.get(fd)?;
    if !f.writable() {
        return Err(Errno::EINVAL);
    }
    f.file.truncate(len)?;
    Ok(0)
}

/// getdents64(fd, dirp, count):ディレクトリの中身を `struct linux_dirent64` の並びで返す。
/// 読む位置(`OpenFile::offset`)は項目の番号。0 と 1 は `.` と `..`。
pub fn getdents64(p: &mut Process, fd: u64, buf: u64, count: u64) -> Result<u64, Errno> {
    let f = p.fds.get(fd)?;
    if f.file.stat().kind != FileKind::Directory {
        return Err(Errno::ENOTDIR);
    }
    let path = f.path.clone().ok_or(Errno::EBADF)?;
    let mut entries = alloc::vec![
        vfs::DirEntry {
            name: alloc::string::String::from("."),
            kind: FileKind::Directory,
            ino: 1
        },
        vfs::DirEntry {
            name: alloc::string::String::from(".."),
            kind: FileKind::Directory,
            ino: 1
        },
    ];
    entries.extend(p.vfs.readdir(&path)?);
    let mut out = alloc::vec::Vec::new();
    let mut index = *f.offset.lock() as usize;
    while let Some(e) = entries.get(index) {
        // d_ino(8) d_off(8) d_reclen(2) d_type(1) 名前 NUL、8 バイト境界に切り上げ
        let reclen = (19 + e.name.len() + 1).div_ceil(8) * 8;
        if out.len() + reclen > count as usize {
            if out.is_empty() {
                return Err(Errno::EINVAL); // 1 項目も入らない
            }
            break;
        }
        let d_type: u8 = match e.kind {
            FileKind::Directory => 4,
            FileKind::Regular => 8,
            FileKind::CharDevice => 2,
            FileKind::Fifo => 1,
            FileKind::Symlink => 10,
        };
        out.extend_from_slice(&e.ino.to_le_bytes());
        out.extend_from_slice(&((index + 1) as u64).to_le_bytes());
        out.extend_from_slice(&(reclen as u16).to_le_bytes());
        out.push(d_type);
        out.extend_from_slice(e.name.as_bytes());
        out.resize(out.len() + (reclen - 19 - e.name.len()), 0);
        index += 1;
    }
    p.mem.copy_to_user(VirtAddr(buf), &out)?;
    *f.offset.lock() = index as u64;
    Ok(out.len() as u64)
}

fn readable(p: &Process, fd: u64) -> Result<Arc<OpenFile>, Errno> {
    let f = p.fds.get(fd)?;
    if f.readable() {
        Ok(f)
    } else {
        Err(Errno::EBADF)
    }
}

fn writable(p: &Process, fd: u64) -> Result<Arc<OpenFile>, Errno> {
    let f = p.fds.get(fd)?;
    if f.writable() {
        Ok(f)
    } else {
        Err(Errno::EBADF)
    }
}

/// ファイルから読んでユーザーの `buf` に写す。位置を進める。
fn read_into(p: &mut Process, f: &OpenFile, buf: u64, count: u64) -> Result<u64, Errno> {
    let mut chunk = vec![0u8; CHUNK.min(count as usize)];
    let mut done = 0;
    while done < count {
        let want = (count - done).min(CHUNK as u64) as usize;
        let offset = *f.offset.lock();
        let n = f.file.read(offset, &mut chunk[..want])?;
        if n == 0 {
            break;
        }
        p.mem.copy_to_user(VirtAddr(buf + done), &chunk[..n])?;
        *f.offset.lock() = offset + n as u64;
        done += n as u64;
        if n < want {
            break;
        }
    }
    Ok(done)
}

/// ユーザーの `buf` からファイルに書く。位置を進める。
fn write_from(p: &mut Process, f: &OpenFile, buf: u64, count: u64) -> Result<u64, Errno> {
    let mut chunk = vec![0u8; CHUNK.min(count as usize)];
    let mut done = 0;
    while done < count {
        let n = (count - done).min(CHUNK as u64) as usize;
        p.mem
            .copy_from_user(VirtAddr(buf + done), &mut chunk[..n])?;
        let offset = *f.offset.lock();
        let written = f.file.write(offset, &chunk[..n])?;
        *f.offset.lock() = offset + written as u64;
        done += written as u64;
        if written < n {
            break;
        }
    }
    Ok(done)
}

/// read(fd, buf, count)
pub fn read(p: &mut Process, fd: u64, buf: u64, count: u64) -> Result<u64, Errno> {
    let f = readable(p, fd)?;
    read_into(p, &f, buf, count)
}

/// write(fd, buf, count)
pub fn write(p: &mut Process, fd: u64, buf: u64, count: u64) -> Result<u64, Errno> {
    let f = writable(p, fd)?;
    write_from(p, &f, buf, count)
}

/// ユーザーの iovec 配列(`{ base: u64, len: u64 }` の並び)を読む。
fn iovecs(p: &mut Process, iov: u64, count: u64) -> Result<vec::Vec<(u64, u64)>, Errno> {
    if count > 1024 {
        return Err(Errno::EINVAL);
    }
    let mut raw = vec![0u8; count as usize * 16];
    p.mem.copy_from_user(VirtAddr(iov), &mut raw)?;
    Ok(raw
        .chunks(16)
        .map(|c| {
            let w = |i: usize| u64::from_le_bytes(c[i..i + 8].try_into().unwrap());
            (w(0), w(8))
        })
        .collect())
}

/// readv(fd, iov, iovcnt)
pub fn readv(p: &mut Process, fd: u64, iov: u64, count: u64) -> Result<u64, Errno> {
    let f = readable(p, fd)?;
    let mut total = 0;
    for (base, len) in iovecs(p, iov, count)? {
        let n = read_into(p, &f, base, len)?;
        total += n;
        if n < len {
            break;
        }
    }
    Ok(total)
}

/// writev(fd, iov, iovcnt)
pub fn writev(p: &mut Process, fd: u64, iov: u64, count: u64) -> Result<u64, Errno> {
    let f = writable(p, fd)?;
    let mut total = 0;
    for (base, len) in iovecs(p, iov, count)? {
        total += write_from(p, &f, base, len)?;
    }
    Ok(total)
}

/// lseek(fd, offset, whence)
pub fn lseek(p: &mut Process, fd: u64, offset: i64, whence: u64) -> Result<u64, Errno> {
    let f = p.fds.get(fd)?;
    let st = f.file.stat();
    if st.kind != FileKind::Regular {
        return Err(Errno::ESPIPE);
    }
    let mut pos = f.offset.lock();
    let base = match whence {
        0 => 0,
        1 => *pos as i64,
        2 => st.size as i64,
        _ => return Err(Errno::EINVAL),
    };
    let new = base
        .checked_add(offset)
        .filter(|&n| n >= 0)
        .ok_or(Errno::EINVAL)?;
    *pos = new as u64;
    Ok(new as u64)
}

/// Linux AArch64 の `struct stat`(128 バイト)を作る。
fn stat_bytes(st: &Stat) -> [u8; 128] {
    let mut b = [0u8; 128];
    let kind = match st.kind {
        FileKind::Regular => 0o100000,
        FileKind::Directory => 0o040000,
        FileKind::CharDevice => 0o020000,
        FileKind::Fifo => 0o010000,
        FileKind::Symlink => 0o120000,
    };
    b[8..16].copy_from_slice(&st.ino.to_le_bytes()); // st_ino
    // st_atim、st_mtim、st_ctim(それぞれ秒 8 バイト + ナノ秒 8 バイト)
    for (at, t) in [(72, st.atime), (88, st.mtime), (104, st.ctime)] {
        b[at..at + 8].copy_from_slice(&t.sec.to_le_bytes());
        b[at + 8..at + 16].copy_from_slice(&(t.nsec as u64).to_le_bytes());
    }
    b[16..20].copy_from_slice(&(kind | st.mode).to_le_bytes()); // st_mode
    b[20..24].copy_from_slice(&1u32.to_le_bytes()); // st_nlink
    b[24..28].copy_from_slice(&st.uid.to_le_bytes()); // st_uid
    b[28..32].copy_from_slice(&st.gid.to_le_bytes()); // st_gid
    b[48..56].copy_from_slice(&st.size.to_le_bytes()); // st_size
    b[56..60].copy_from_slice(&4096u32.to_le_bytes()); // st_blksize
    b[64..72].copy_from_slice(&st.size.div_ceil(512).to_le_bytes()); // st_blocks
    b
}

/// fstat(fd, statbuf)
pub fn fstat(p: &mut Process, fd: u64, buf: u64) -> Result<u64, Errno> {
    let st = p.fds.get(fd)?.file.stat();
    p.mem.copy_to_user(VirtAddr(buf), &stat_bytes(&st))?;
    Ok(0)
}

/// newfstatat(dirfd, path, statbuf, flags)
pub fn newfstatat(
    p: &mut Process,
    dirfd: u64,
    path: u64,
    buf: u64,
    flags: u64,
) -> Result<u64, Errno> {
    if flags & AT_EMPTY_PATH != 0
        && p.mem
            .read_cstr(VirtAddr(path), 1)
            .is_ok_and(|s| s.is_empty())
    {
        return fstat(p, dirfd, buf);
    }
    const AT_SYMLINK_NOFOLLOW: u64 = 0x100;
    let path = path_arg(p, dirfd, path)?;
    let file = if flags & AT_SYMLINK_NOFOLLOW != 0 {
        p.vfs.lookup_nofollow(&path)?
    } else {
        p.vfs.lookup(&path)?
    };
    p.mem
        .copy_to_user(VirtAddr(buf), &stat_bytes(&file.stat()))?;
    Ok(0)
}

/// ioctl(fd, cmd, arg)。端末の設定(termios)と大きさの問い合わせ。
pub fn ioctl(p: &mut Process, fd: u64, cmd: u64, arg: u64) -> Result<u64, Errno> {
    let f = p.fds.get(fd)?;
    if !f.file.is_tty() {
        return Err(Errno::ENOTTY);
    }
    match cmd {
        TCGETS => {
            let t = f.file.termios().ok_or(Errno::ENOTTY)?;
            p.mem.copy_to_user(VirtAddr(arg), &t.to_bytes())?;
            Ok(0)
        }
        TCSETS | TCSETSW | TCSETSF => {
            let mut b = [0u8; TERMIOS_SIZE];
            p.mem.copy_from_user(VirtAddr(arg), &mut b)?;
            f.file.set_termios(Termios::from_bytes(&b))?;
            Ok(0)
        }
        TIOCGWINSZ => {
            // struct winsize { rows, cols, xpixel, ypixel }(各 u16)
            let ws: [u16; 4] = [24, 80, 0, 0];
            let bytes: vec::Vec<u8> = ws.iter().flat_map(|v| v.to_le_bytes()).collect();
            p.mem.copy_to_user(VirtAddr(arg), &bytes)?;
            Ok(0)
        }
        _ => Err(Errno::ENOTTY),
    }
}

/// getcwd(buf, size)。戻り値は NUL を含む長さ。
pub fn getcwd(p: &mut Process, buf: u64, size: u64) -> Result<u64, Errno> {
    let mut bytes = p.cwd.clone().into_bytes();
    bytes.push(0);
    if bytes.len() as u64 > size {
        return Err(Errno::ERANGE);
    }
    p.mem.copy_to_user(VirtAddr(buf), &bytes)?;
    Ok(bytes.len() as u64)
}

/// readlinkat(dirfd, path, buf, size)。シンボリックリンクはまだないので、あるパスなら EINVAL。
pub fn readlinkat(
    p: &mut Process,
    dirfd: u64,
    path: u64,
    buf: u64,
    len: u64,
) -> Result<u64, Errno> {
    let path = path_arg(p, dirfd, path)?;
    let target = p.vfs.lookup_nofollow(&path)?.readlink()?;
    // NUL は付けず、入りきらなければ切り詰める
    let n = target.len().min(len as usize);
    p.mem.copy_to_user(VirtAddr(buf), &target.as_bytes()[..n])?;
    Ok(n as u64)
}

/// chmod の共通部分:持ち主か root だけが変えられる
fn change_mode(p: &Process, file: &dyn vfs::File, mode: u64) -> Result<u64, Errno> {
    if p.cred.euid != 0 && p.cred.euid != file.stat().uid {
        return Err(Errno::EPERM);
    }
    file.set_mode(mode as u32 & 0o7777)?;
    Ok(0)
}

/// chown の共通部分:root だけが変えられる。-1(u32::MAX)はその値を変えない
fn change_owner(p: &Process, file: &dyn vfs::File, uid: u64, gid: u64) -> Result<u64, Errno> {
    if p.cred.euid != 0 {
        return Err(Errno::EPERM);
    }
    let st = file.stat();
    let pick = |v: u64, old: u32| if v as u32 == u32::MAX { old } else { v as u32 };
    file.set_owner(pick(uid, st.uid), pick(gid, st.gid))?;
    Ok(0)
}

/// fchmodat(dirfd, path, mode, flags)
pub fn fchmodat(p: &mut Process, dirfd: u64, path: u64, mode: u64) -> Result<u64, Errno> {
    let path = path_arg(p, dirfd, path)?;
    let file = p.vfs.lookup(&path)?;
    change_mode(p, &*file, mode)
}

/// fchmod(fd, mode)
pub fn fchmod(p: &mut Process, fd: u64, mode: u64) -> Result<u64, Errno> {
    let file = p.fds.get(fd)?.file.clone();
    change_mode(p, &*file, mode)
}

/// fchownat(dirfd, path, uid, gid, flags)。AT_SYMLINK_NOFOLLOW ならリンクそのもの(lchown)
pub fn fchownat(
    p: &mut Process,
    dirfd: u64,
    path: u64,
    uid: u64,
    gid: u64,
    flags: u64,
) -> Result<u64, Errno> {
    const AT_SYMLINK_NOFOLLOW: u64 = 0x100;
    let path = path_arg(p, dirfd, path)?;
    let file = if flags & AT_SYMLINK_NOFOLLOW != 0 {
        p.vfs.lookup_nofollow(&path)?
    } else {
        p.vfs.lookup(&path)?
    };
    change_owner(p, &*file, uid, gid)
}

/// fchown(fd, uid, gid)
pub fn fchown(p: &mut Process, fd: u64, uid: u64, gid: u64) -> Result<u64, Errno> {
    let file = p.fds.get(fd)?.file.clone();
    change_owner(p, &*file, uid, gid)
}

/// utimensat(dirfd, path, times, flags)。path が NULL なら dirfd のファイル(futimens)。
/// times が NULL なら両方とも今。ナノ秒が UTIME_NOW なら今、UTIME_OMIT なら変えない。
/// 時刻を指定できるのは持ち主か root。今にするだけなら、書き込める人もよい。
pub fn utimensat(
    p: &mut Process,
    dirfd: u64,
    path: u64,
    times: u64,
    flags: u64,
) -> Result<u64, Errno> {
    const UTIME_NOW: u64 = 0x3fff_ffff;
    const UTIME_OMIT: u64 = 0x3fff_fffe;
    const AT_SYMLINK_NOFOLLOW: u64 = 0x100;
    let file = if path == 0 {
        p.fds.get(dirfd)?.file.clone()
    } else {
        let path = path_arg(p, dirfd, path)?;
        if flags & AT_SYMLINK_NOFOLLOW != 0 {
            p.vfs.lookup_nofollow(&path)?
        } else {
            p.vfs.lookup(&path)?
        }
    };
    let now = vfs::Timespec::now();
    let mut set = [Some(now), Some(now)];
    let mut explicit = false;
    if times != 0 {
        let mut b = [0u8; 32];
        p.mem.copy_from_user(VirtAddr(times), &mut b)?;
        for (i, slot) in set.iter_mut().enumerate() {
            let sec = i64::from_le_bytes(b[i * 16..i * 16 + 8].try_into().unwrap());
            let nsec = u64::from_le_bytes(b[i * 16 + 8..i * 16 + 16].try_into().unwrap());
            *slot = match nsec {
                UTIME_NOW => Some(now),
                UTIME_OMIT => None,
                n if n < 1_000_000_000 => {
                    explicit = true;
                    Some(vfs::Timespec {
                        sec,
                        nsec: n as u32,
                    })
                }
                _ => return Err(Errno::EINVAL),
            };
        }
    }
    let st = file.stat();
    let owner = p.cred.euid == 0 || p.cred.euid == st.uid;
    if !owner {
        if explicit {
            return Err(Errno::EPERM);
        }
        p.cred.check(&st, Access::WRITE)?;
    }
    file.set_times(set[0], set[1])?;
    Ok(0)
}

/// 新しく作ったものを、作ったプロセスの持ち主(実効 uid と gid)にする
fn own_new(p: &Process, file: &dyn vfs::File) -> Result<(), Errno> {
    if p.cred.euid != 0 || p.cred.egid != 0 {
        file.set_owner(p.cred.euid, p.cred.egid)?;
    }
    Ok(())
}

/// symlinkat(target, newdirfd, linkpath)
pub fn symlinkat(p: &mut Process, target: u64, dirfd: u64, link: u64) -> Result<u64, Errno> {
    let raw = p.mem.read_cstr(VirtAddr(target), PATH_MAX)?;
    let target = core::str::from_utf8(&raw).map_err(|_| Errno::EINVAL)?;
    if target.is_empty() {
        return Err(Errno::ENOENT);
    }
    let target = alloc::string::String::from(target);
    let link = path_arg(p, dirfd, link)?;
    let dir = p.vfs.lookup(&vfs::normalize(&link, ".."))?;
    p.cred.check(&dir.stat(), Access::WRITE | Access::EXEC)?;
    p.vfs.symlink(&target, &link)?;
    own_new(p, &*p.vfs.lookup_nofollow(&link)?)?;
    Ok(0)
}

/// chdir(path)
pub fn chdir(p: &mut Process, path: u64) -> Result<u64, Errno> {
    let path = path_arg(p, AT_FDCWD as u64, path)?;
    let st = p.vfs.lookup(&path)?.stat();
    if st.kind != FileKind::Directory {
        return Err(Errno::ENOTDIR);
    }
    p.cred.check(&st, Access::EXEC)?;
    p.cwd = path;
    Ok(0)
}

const O_CLOEXEC: u64 = 0o2000000;

/// pipe2(fds, flags):パイプを作り、読む側と書く側の fd を `int fds[2]` に書く。
pub fn pipe2(p: &mut Process, fds: u64, flags: u64) -> Result<u64, Errno> {
    const O_NONBLOCK: u64 = 0o4000;
    if flags & !(O_CLOEXEC | O_NONBLOCK) != 0 {
        return Err(Errno::EINVAL);
    }
    let (reader, writer) = crate::fs::pipe::new();
    let r = p
        .fds
        .insert(OpenFile::new(reader, crate::fs::vfs::O_RDONLY))?;
    let w = match p
        .fds
        .insert(OpenFile::new(writer, crate::fs::vfs::O_WRONLY))
    {
        Ok(w) => w,
        Err(e) => {
            let _ = p.fds.close(r as u64);
            return Err(e);
        }
    };
    let mut b = [0u8; 8];
    b[..4].copy_from_slice(&(r as i32).to_le_bytes());
    b[4..].copy_from_slice(&(w as i32).to_le_bytes());
    if let Err(e) = p.mem.copy_to_user(VirtAddr(fds), &b) {
        let _ = p.fds.close(r as u64);
        let _ = p.fds.close(w as u64);
        return Err(e);
    }
    let cloexec = flags & O_CLOEXEC != 0;
    p.fds.set_cloexec(r as u64, cloexec)?;
    p.fds.set_cloexec(w as u64, cloexec)?;
    Ok(0)
}
const FD_CLOEXEC: u64 = 1;

/// dup(fd)
pub fn dup(p: &mut Process, fd: u64) -> Result<u64, Errno> {
    let f = p.fds.get(fd)?;
    Ok(p.fds.insert(f)? as u64)
}

/// dup3(oldfd, newfd, flags)
pub fn dup3(p: &mut Process, old: u64, new: u64, flags: u64) -> Result<u64, Errno> {
    if old == new {
        return Err(Errno::EINVAL);
    }
    let f = p.fds.get(old)?;
    p.fds.insert_at(new as usize, f)?;
    p.fds.set_cloexec(new, flags & O_CLOEXEC != 0)?;
    Ok(new)
}

/// fcntl(fd, cmd, arg)
pub fn fcntl(p: &mut Process, fd: u64, cmd: u64, arg: u64) -> Result<u64, Errno> {
    const F_DUPFD: u64 = 0;
    const F_GETFD: u64 = 1;
    const F_SETFD: u64 = 2;
    const F_GETFL: u64 = 3;
    const F_SETFL: u64 = 4;
    const F_DUPFD_CLOEXEC: u64 = 1030;
    let f = p.fds.get(fd)?;
    match cmd {
        F_DUPFD | F_DUPFD_CLOEXEC => {
            let new = p.fds.insert_from(arg as usize, f)? as u64;
            p.fds.set_cloexec(new, cmd == F_DUPFD_CLOEXEC)?;
            Ok(new)
        }
        F_GETFD => Ok(if p.fds.is_cloexec(fd) { FD_CLOEXEC } else { 0 }),
        F_SETFD => {
            p.fds.set_cloexec(fd, arg & FD_CLOEXEC != 0)?;
            Ok(0)
        }
        F_GETFL => Ok(f.flags as u64),
        // O_NONBLOCK などはまだ扱わない(受け付けるだけ)
        F_SETFL => Ok(0),
        _ => Err(Errno::EINVAL),
    }
}

/// faccessat(dirfd, path, mode):実ユーザー ID で、存在と権限を確かめる。
pub fn faccessat(p: &mut Process, dirfd: u64, path: u64, mode: u64) -> Result<u64, Errno> {
    let path = path_arg(p, dirfd, path)?;
    let st = p.vfs.lookup(&path)?.stat();
    if mode & 7 == 0 {
        return Ok(0); // F_OK:あるかどうかだけ
    }
    let mut want = Access::NONE;
    for (bit, access) in [(4, Access::READ), (2, Access::WRITE), (1, Access::EXEC)] {
        if mode & bit != 0 {
            want = want | access;
        }
    }
    // access は実効 ID ではなく実 ID で判定する
    let mut real = p.cred.clone();
    real.euid = real.uid;
    real.egid = real.gid;
    real.check(&st, want)?;
    Ok(0)
}

/// pread64(fd, buf, count, offset):位置を指定して読む(ファイルの位置は動かさない)
pub fn pread64(p: &mut Process, fd: u64, buf: u64, count: u64, offset: u64) -> Result<u64, Errno> {
    let f = readable(p, fd)?;
    let mut chunk = vec![0u8; CHUNK.min(count as usize)];
    let n = f.file.read(offset, &mut chunk)?;
    p.mem.copy_to_user(VirtAddr(buf), &chunk[..n])?;
    Ok(n as u64)
}

/// pwrite64(fd, buf, count, offset):位置を指定して書く(ファイルの位置は動かさない)
pub fn pwrite64(p: &mut Process, fd: u64, buf: u64, count: u64, offset: u64) -> Result<u64, Errno> {
    let f = writable(p, fd)?;
    let mut chunk = vec![0u8; CHUNK.min(count as usize)];
    p.mem.copy_from_user(VirtAddr(buf), &mut chunk)?;
    Ok(f.file.write(offset, &chunk)? as u64)
}
