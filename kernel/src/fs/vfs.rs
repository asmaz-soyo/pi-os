//! VFS(仮想ファイルシステム)。
//!
//! ファイル、ディレクトリ、端末など、中身の違うものを [`File`] という同じ形で扱う。
//! システムコールは `File` しか知らないので、ファイルシステムやデバイスを足しても変わらない。
//!
//! ```text
//! fd(数字) → FdTable → OpenFile(位置とフラグ) → Arc<dyn File>(実体)
//! ```

use crate::Errno;
use crate::sync::SpinLock;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

// open のフラグ(Linux と同じ値)
pub const O_RDONLY: u32 = 0;
pub const O_WRONLY: u32 = 1;
pub const O_RDWR: u32 = 2;
pub const O_ACCMODE: u32 = 3;
pub const O_CREAT: u32 = 0o100;
pub const O_TRUNC: u32 = 0o1000;
pub const O_DIRECTORY: u32 = 0o200000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Regular,
    Directory,
    CharDevice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    pub kind: FileKind,
    /// パーミッション(下位 9 ビット)
    pub mode: u32,
    pub size: u64,
    pub ino: u64,
}

/// ファイルの実体。読み書きは位置を引数で受け取る(位置は `OpenFile` が持つ)。
pub trait File: Send + Sync {
    fn stat(&self) -> Stat;

    fn read(&self, _offset: u64, _buf: &mut [u8]) -> Result<usize, Errno> {
        Err(Errno::EINVAL)
    }

    fn write(&self, _offset: u64, _buf: &[u8]) -> Result<usize, Errno> {
        Err(Errno::EINVAL)
    }

    /// 端末か(`isatty`、TIOCGWINSZ に答えるか)
    fn is_tty(&self) -> bool {
        false
    }

    /// 中身がメモリにそのままあれば、その全体(mmap で写すため)
    fn contents(&self) -> Option<&[u8]> {
        None
    }
}

/// 開いたファイル。読み書きの位置とフラグを持つ。
pub struct OpenFile {
    pub file: Arc<dyn File>,
    pub flags: u32,
    pub offset: SpinLock<u64>,
}

impl OpenFile {
    pub fn new(file: Arc<dyn File>, flags: u32) -> Arc<Self> {
        Arc::new(Self {
            file,
            flags,
            offset: SpinLock::new(0),
        })
    }

    pub fn readable(&self) -> bool {
        self.flags & O_ACCMODE != O_WRONLY
    }

    pub fn writable(&self) -> bool {
        self.flags & O_ACCMODE != O_RDONLY
    }
}

/// ファイル記述子の表。
pub struct FdTable {
    fds: Vec<Option<Arc<OpenFile>>>,
}

/// 1つのプロセスが開けるファイルの数
pub const MAX_FDS: usize = 256;

impl FdTable {
    pub const fn new() -> Self {
        Self { fds: Vec::new() }
    }

    /// 空いている一番小さい番号に入れる。
    pub fn insert(&mut self, file: Arc<OpenFile>) -> Result<usize, Errno> {
        if let Some(fd) = self.fds.iter().position(Option::is_none) {
            self.fds[fd] = Some(file);
            return Ok(fd);
        }
        if self.fds.len() >= MAX_FDS {
            return Err(Errno::EMFILE);
        }
        self.fds.push(Some(file));
        Ok(self.fds.len() - 1)
    }

    pub fn get(&self, fd: u64) -> Result<Arc<OpenFile>, Errno> {
        self.fds
            .get(fd as usize)
            .and_then(Clone::clone)
            .ok_or(Errno::EBADF)
    }

    pub fn close(&mut self, fd: u64) -> Result<(), Errno> {
        let slot = self.fds.get_mut(fd as usize).ok_or(Errno::EBADF)?;
        slot.take().map(|_| ()).ok_or(Errno::EBADF)
    }
}

impl Default for FdTable {
    fn default() -> Self {
        Self::new()
    }
}

/// `cwd` を基準に `path` を絶対パスにし、`.` と `..` と重なった `/` を取り除く。
pub fn normalize(cwd: &str, path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let full = if path.starts_with('/') {
        [path, ""]
    } else {
        [cwd, path]
    };
    for piece in full.iter().flat_map(|p| p.split('/')) {
        match piece {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            name => parts.push(name),
        }
    }
    let mut out = String::from("/");
    out.push_str(&parts.join("/"));
    out
}

/// パスから実体を探すファイルシステム。
pub trait FileSystem: Send + Sync {
    /// `path` はこのファイルシステムの中の絶対パス(`/` で始まる)
    fn lookup(&self, path: &str) -> Result<Arc<dyn File>, Errno>;
}

/// マウントの表。長いマウントポイントから順に調べる。
pub struct Vfs {
    mounts: Vec<(String, Arc<dyn FileSystem>)>,
}

impl Vfs {
    pub fn new(root: Arc<dyn FileSystem>) -> Self {
        Self {
            mounts: alloc::vec![(String::from("/"), root)],
        }
    }

    pub fn mount(&mut self, at: &str, fs: Arc<dyn FileSystem>) {
        self.mounts.push((String::from(at), fs));
        self.mounts
            .sort_by_key(|(p, _)| core::cmp::Reverse(p.len()));
    }

    /// 絶対パスの実体を探す。
    pub fn lookup(&self, path: &str) -> Result<Arc<dyn File>, Errno> {
        for (at, fs) in &self.mounts {
            let rest = if at == "/" {
                Some(path)
            } else {
                path.strip_prefix(at.as_str())
                    .filter(|r| r.is_empty() || r.starts_with('/'))
            };
            if let Some(rest) = rest {
                let inner = if rest.is_empty() { "/" } else { rest };
                return fs.lookup(inner);
            }
        }
        Err(Errno::ENOENT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Errno;

    #[test]
    fn normalize_resolves_dots_and_slashes() {
        assert_eq!(normalize("/", "etc//motd"), "/etc/motd");
        assert_eq!(normalize("/bin", "../etc/./motd"), "/etc/motd");
        assert_eq!(normalize("/", "/../../x"), "/x");
        assert_eq!(normalize("/a/b", "."), "/a/b");
        assert_eq!(normalize("/a/b", "/"), "/");
    }

    struct Dummy;
    impl File for Dummy {
        fn stat(&self) -> Stat {
            Stat {
                kind: FileKind::Regular,
                mode: 0o644,
                size: 0,
                ino: 1,
            }
        }
    }

    #[test]
    fn fd_table_hands_out_the_lowest_free_number() {
        let mut fds = FdTable::new();
        let f: alloc::sync::Arc<dyn File> = alloc::sync::Arc::new(Dummy);
        assert_eq!(fds.insert(OpenFile::new(f.clone(), O_RDONLY)), Ok(0));
        assert_eq!(fds.insert(OpenFile::new(f.clone(), O_RDONLY)), Ok(1));
        assert_eq!(fds.insert(OpenFile::new(f.clone(), O_RDONLY)), Ok(2));
        fds.close(1).unwrap();
        assert_eq!(fds.insert(OpenFile::new(f.clone(), O_RDONLY)), Ok(1));
        assert_eq!(fds.close(9), Err(Errno::EBADF));
        assert!(fds.get(0).is_ok());
        assert_eq!(fds.get(5).err(), Some(Errno::EBADF));
    }

    #[test]
    fn default_file_operations_are_errors() {
        let mut buf = [0u8; 4];
        assert_eq!(Dummy.read(0, &mut buf), Err(Errno::EINVAL));
        assert_eq!(Dummy.write(0, b"x"), Err(Errno::EINVAL));
        assert!(!Dummy.is_tty());
    }
}
