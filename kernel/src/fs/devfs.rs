//! デバイスファイル(`/dev/console`、`/dev/null`)。

use crate::Errno;
use crate::fs::vfs::{DirEntry, File, FileKind, FileSystem, Stat};
use crate::sync::SpinLock;
use crate::tty::{Termios, Tty};
use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use hal::console::Console;

/// 端末(UART + 行規律)。
pub struct ConsoleFile {
    dev: SpinLock<Box<dyn Console + Send>>,
    tty: SpinLock<Tty>,
}

impl ConsoleFile {
    pub fn new(dev: Box<dyn Console + Send>) -> Self {
        Self {
            dev: SpinLock::new(dev),
            tty: SpinLock::new(Tty::new()),
        }
    }

    /// UART に届いているバイトを行規律に渡す(エコーは UART に書く)。
    fn poll_input(&self) {
        let mut dev = self.dev.lock();
        let mut tty = self.tty.lock();
        let mut echo = Vec::new();
        while let Some(byte) = dev.read_byte() {
            tty.input(byte, &mut |out| echo.extend_from_slice(out));
        }
        if !echo.is_empty() {
            dev.write_bytes(&echo);
        }
    }
}

impl File for ConsoleFile {
    fn stat(&self) -> Stat {
        Stat {
            kind: FileKind::CharDevice,
            mode: 0o620,
            size: 0,
            ino: 0x1_0001,
            uid: 0,
            gid: 0,
        }
    }

    /// 1 行(raw モードなら届いた分)が読めるまで待つ。
    ///
    /// 待つ間はほかのタスクに譲る(UART を繰り返し確かめる)。
    /// UART の受信割り込みで起こしてもらう形にするのは、後で。
    fn read(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        loop {
            self.poll_input();
            if let Some(n) = self.tty.lock().read(buf) {
                return Ok(n);
            }
            crate::sched::relax();
        }
    }

    fn write(&self, _offset: u64, buf: &[u8]) -> Result<usize, Errno> {
        self.dev.lock().write_bytes(buf);
        Ok(buf.len())
    }

    fn is_tty(&self) -> bool {
        true
    }

    fn poll_readable(&self) -> bool {
        self.poll_input();
        self.tty.lock().readable()
    }

    fn termios(&self) -> Option<Termios> {
        Some(self.tty.lock().termios())
    }

    fn set_termios(&self, t: Termios) -> Result<(), Errno> {
        self.tty.lock().set_termios(t);
        Ok(())
    }
}

/// 書いたものを捨て、読むと常に終わり。
pub struct NullFile;

impl File for NullFile {
    fn stat(&self) -> Stat {
        Stat {
            kind: FileKind::CharDevice,
            mode: 0o666,
            size: 0,
            ino: 0x1_0002,
            uid: 0,
            gid: 0,
        }
    }

    fn read(&self, _offset: u64, _buf: &mut [u8]) -> Result<usize, Errno> {
        Ok(0)
    }

    fn write(&self, _offset: u64, buf: &[u8]) -> Result<usize, Errno> {
        Ok(buf.len())
    }
}

struct DevDir;

impl File for DevDir {
    fn stat(&self) -> Stat {
        Stat {
            kind: FileKind::Directory,
            mode: 0o755,
            size: 0,
            ino: 0x1_0000,
            uid: 0,
            gid: 0,
        }
    }
}

/// `/dev` に置くファイルシステム。
pub struct Devfs {
    console: Arc<ConsoleFile>,
}

impl Devfs {
    pub fn new(console: Arc<ConsoleFile>) -> Self {
        Self { console }
    }
}

impl FileSystem for Devfs {
    fn lookup(&self, path: &str) -> Result<Arc<dyn File>, Errno> {
        match path {
            "/" => Ok(Arc::new(DevDir)),
            "/console" | "/tty" => Ok(self.console.clone()),
            "/null" => Ok(Arc::new(NullFile)),
            "/gpio" => Ok(Arc::new(DevDir)),
            other if other.starts_with("/gpio/") => {
                let line: usize = other[6..].parse().map_err(|_| Errno::ENOENT)?;
                if line >= crate::gpio::LINES {
                    return Err(Errno::ENOENT);
                }
                Ok(Arc::new(crate::gpio::GpioFile::new(
                    &crate::gpio::GPIO,
                    line,
                )))
            }
            other => {
                // 登録されたブロックデバイス(/dev/vda など)
                let disk =
                    crate::block::find(other.trim_start_matches('/')).ok_or(Errno::ENOENT)?;
                Ok(Arc::new(crate::block::BlockFile::new(disk)))
            }
        }
    }

    fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, Errno> {
        if path != "/" {
            return Err(Errno::ENOTDIR);
        }
        let mut entries: Vec<DirEntry> = ["console", "null", "tty"]
            .iter()
            .map(|name| DirEntry {
                name: alloc::string::String::from(*name),
                kind: FileKind::CharDevice,
                ino: 0x1_0001,
            })
            .collect();
        for name in crate::block::names() {
            entries.push(DirEntry {
                name,
                kind: FileKind::Regular,
                ino: 0x5_0000,
            });
        }
        Ok(entries)
    }
}
