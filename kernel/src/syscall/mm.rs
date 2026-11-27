//! メモリ関係のシステムコール。

use crate::Errno;
use crate::process::{MMAP_BASE, Process};
use alloc::vec;
use hal::addr::{PAGE_SIZE, VirtAddr};
use hal::mmu::MapFlags;

const PROT_WRITE: u64 = 2;
const PROT_EXEC: u64 = 4;
const PROT_READ: u64 = 1;
const MAP_FIXED: u64 = 0x10;
const MAP_ANONYMOUS: u64 = 0x20;

fn page_up(x: u64) -> u64 {
    x.div_ceil(PAGE_SIZE) * PAGE_SIZE
}

fn flags_of(prot: u64) -> Option<MapFlags> {
    if prot & (PROT_READ | PROT_WRITE | PROT_EXEC) == 0 {
        return None; // PROT_NONE:場所だけ確保して、ページは置かない
    }
    Some(MapFlags {
        write: prot & PROT_WRITE != 0,
        exec: prot & PROT_EXEC != 0,
        user: true,
        device: false,
    })
}

/// brk(addr)。ヒープの終わりを `addr` にする。失敗しても(Linux と同じく)今の値を返す。
pub fn brk(p: &mut Process, addr: u64) -> u64 {
    let start = p.entry.brk;
    let brk = p.mem.brk();
    if addr < start || addr >= MMAP_BASE {
        return brk;
    }
    let (old_end, new_end) = (page_up(brk), page_up(addr));
    if new_end > old_end {
        if p.mem
            .map_zeroed(VirtAddr(old_end), new_end - old_end, MapFlags::USER_RW)
            .is_err()
        {
            return brk;
        }
    } else if new_end < old_end {
        p.mem.unmap(VirtAddr(new_end), old_end - new_end);
    }
    p.mem.set_brk(addr);
    addr
}

/// mmap(addr, len, prot, flags, fd, offset)
pub fn mmap(
    p: &mut Process,
    addr: u64,
    len: u64,
    prot: u64,
    flags: u64,
    fd: u64,
    offset: u64,
) -> Result<u64, Errno> {
    if len == 0 || !offset.is_multiple_of(PAGE_SIZE) {
        return Err(Errno::EINVAL);
    }
    let len = page_up(len);
    let file = if flags & MAP_ANONYMOUS == 0 {
        Some(p.fds.get(fd)?)
    } else {
        None
    };
    let start = if flags & MAP_FIXED != 0 {
        if !addr.is_multiple_of(PAGE_SIZE) {
            return Err(Errno::EINVAL);
        }
        p.mem.unmap(VirtAddr(addr), len);
        addr
    } else {
        // 場所はカーネルが選ぶ(下から順に。解放した場所はまだ再利用しない)
        // (ヒープと同じく、スレッドどうしで共有する位置)
        let start = p.mem.mmap_next();
        p.mem.set_mmap_next(start + len + PAGE_SIZE); // 隣とくっつかないよう 1 ページ空ける
        start
    };
    let Some(map_flags) = flags_of(prot) else {
        return Ok(start);
    };
    p.mem.map_zeroed(VirtAddr(start), len, map_flags)?;
    if let Some(f) = file {
        // private マップ:今の中身を写す(書き換えてもファイルには戻らない)
        let mut buf = vec![0u8; PAGE_SIZE as usize];
        let mut done = 0;
        while done < len {
            let n = f.file.read(offset + done, &mut buf)?;
            if n == 0 {
                break;
            }
            p.mem.load(VirtAddr(start + done), &buf[..n])?;
            done += n as u64;
        }
    }
    Ok(start)
}

/// munmap(addr, len)
pub fn munmap(p: &mut Process, addr: u64, len: u64) -> Result<u64, Errno> {
    if !addr.is_multiple_of(PAGE_SIZE) || len == 0 {
        return Err(Errno::EINVAL);
    }
    p.mem.unmap(VirtAddr(addr), len);
    Ok(0)
}

/// mprotect(addr, len, prot)
pub fn mprotect(p: &mut Process, addr: u64, len: u64, prot: u64) -> Result<u64, Errno> {
    if !addr.is_multiple_of(PAGE_SIZE) {
        return Err(Errno::EINVAL);
    }
    match flags_of(prot) {
        Some(flags) => p.mem.protect(VirtAddr(addr), len, flags)?,
        None => p.mem.unmap(VirtAddr(addr), len),
    }
    Ok(0)
}
