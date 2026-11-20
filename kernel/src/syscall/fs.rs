//! ファイル関係のシステムコール。

use super::TIOCGWINSZ;
use crate::Errno;
use crate::fs::vfs::{
    self, FileKind, O_ACCMODE, O_CREAT, O_DIRECTORY, O_RDONLY, O_TRUNC, OpenFile, Stat,
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
    if !path.starts_with('/') && dirfd as i64 != AT_FDCWD {
        // ディレクトリの fd を基準にした相対パスは、まだ扱わない
        return Err(Errno::EBADF);
    }
    Ok(vfs::normalize(&p.cwd, path))
}

/// openat(dirfd, path, flags, mode)
pub fn openat(p: &mut Process, dirfd: u64, path: u64, flags: u64) -> Result<u64, Errno> {
    let flags = flags as u32;
    let path = path_arg(p, dirfd, path)?;
    let file = match p.vfs.lookup(&path) {
        Ok(file) => file,
        Err(Errno::ENOENT) if flags & O_CREAT != 0 => return Err(Errno::EROFS),
        Err(e) => return Err(e),
    };
    let st = file.stat();
    if flags & O_DIRECTORY != 0 && st.kind != FileKind::Directory {
        return Err(Errno::ENOTDIR);
    }
    let wants_write = flags & O_ACCMODE != O_RDONLY || flags & (O_CREAT | O_TRUNC) != 0;
    if wants_write && st.kind != FileKind::CharDevice {
        // 今あるファイルシステムはすべて読み出し専用
        return Err(if st.kind == FileKind::Directory {
            Errno::EISDIR
        } else {
            Errno::EROFS
        });
    }
    let fd = p.fds.insert(OpenFile::new(file, flags))?;
    Ok(fd as u64)
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
    };
    b[8..16].copy_from_slice(&st.ino.to_le_bytes()); // st_ino
    b[16..20].copy_from_slice(&(kind | st.mode).to_le_bytes()); // st_mode
    b[20..24].copy_from_slice(&1u32.to_le_bytes()); // st_nlink
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
    let path = path_arg(p, dirfd, path)?;
    let st = p.vfs.lookup(&path)?.stat();
    p.mem.copy_to_user(VirtAddr(buf), &stat_bytes(&st))?;
    Ok(0)
}

/// ioctl(fd, cmd, arg)。今は端末の大きさの問い合わせだけ。
pub fn ioctl(p: &mut Process, fd: u64, cmd: u64, arg: u64) -> Result<u64, Errno> {
    let f = p.fds.get(fd)?;
    if !f.file.is_tty() {
        return Err(Errno::ENOTTY);
    }
    match cmd {
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
pub fn readlinkat(p: &mut Process, dirfd: u64, path: u64) -> Result<u64, Errno> {
    let path = path_arg(p, dirfd, path)?;
    p.vfs.lookup(&path)?;
    Err(Errno::EINVAL)
}
