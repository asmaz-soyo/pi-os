//! デバイスファイル(`/dev/console`、`/dev/null`)。

use crate::Errno;
use crate::fs::vfs::{File, FileKind, FileSystem, Stat};
use crate::sync::SpinLock;
use alloc::boxed::Box;
use alloc::sync::Arc;
use hal::console::Console;

/// 端末(UART)。書くと出力される。入力(UART の受信)はフェーズ2で作るので、今は読むと常に終わり。
pub struct ConsoleFile {
    out: SpinLock<Box<dyn Console + Send>>,
}

impl ConsoleFile {
    pub fn new(out: Box<dyn Console + Send>) -> Self {
        Self {
            out: SpinLock::new(out),
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

    fn read(&self, _offset: u64, _buf: &mut [u8]) -> Result<usize, Errno> {
        Ok(0)
    }

    fn write(&self, _offset: u64, buf: &[u8]) -> Result<usize, Errno> {
        self.out.lock().write_bytes(buf);
        Ok(buf.len())
    }

    fn is_tty(&self) -> bool {
        true
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
            _ => Err(Errno::ENOENT),
        }
    }
}
