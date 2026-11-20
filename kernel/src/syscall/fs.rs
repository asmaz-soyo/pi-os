//! ファイル関係のシステムコール。

use crate::Errno;
use crate::process::Process;
use alloc::vec;
use hal::addr::VirtAddr;

/// 一度にユーザーから写す量の上限
const CHUNK: usize = 4096;

/// write(fd, buf, count)
pub fn write(p: &mut Process, fd: u64, buf: u64, count: u64) -> Result<u64, Errno> {
    if fd != 1 && fd != 2 {
        return Err(Errno::EBADF);
    }
    let mut chunk = vec![0u8; CHUNK.min(count as usize)];
    let mut done = 0;
    while done < count {
        let n = (count - done).min(CHUNK as u64) as usize;
        p.mem
            .copy_from_user(VirtAddr(buf + done), &mut chunk[..n])?;
        p.stdout.write_bytes(&chunk[..n]);
        done += n as u64;
    }
    Ok(count)
}
