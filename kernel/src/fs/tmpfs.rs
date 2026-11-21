//! tmpfs:メモリの上の、書けるファイルシステム(`/tmp`)。
//!
//! パスをキーにした表で、ファイルとディレクトリを持つ。中身は `Vec<u8>`。
//! 消したファイルも、開いている間は中身が残る(`Arc` で参照されているため)。

use crate::Errno;
use crate::fs::vfs::{DirEntry, File, FileKind, FileSystem, Stat, Timespec};
use crate::sync::SpinLock;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

/// 持ち主と権限(chmod と chown で変わる)
#[derive(Clone, Copy)]
struct Meta {
    mode: u32,
    uid: u32,
    gid: u32,
    /// 名前の数(ハードリンク)
    nlink: u32,
    atime: Timespec,
    mtime: Timespec,
    ctime: Timespec,
}

impl Meta {
    fn new(mode: u32) -> SpinLock<Meta> {
        let now = Timespec::now();
        SpinLock::new(Meta {
            mode: mode & 0o7777,
            uid: 0,
            gid: 0,
            nlink: 1,
            atime: now,
            mtime: now,
            ctime: now,
        })
    }

    fn stat(meta: &SpinLock<Meta>, kind: FileKind, size: u64, ino: u64) -> Stat {
        let m = *meta.lock();
        Stat {
            kind,
            mode: m.mode,
            size,
            ino,
            uid: m.uid,
            gid: m.gid,
            nlink: m.nlink,
            atime: m.atime,
            mtime: m.mtime,
            ctime: m.ctime,
        }
    }

    /// 中身を変えた(mtime と ctime を今に)
    fn modified(meta: &SpinLock<Meta>) {
        let now = Timespec::now();
        let mut m = meta.lock();
        m.mtime = now;
        m.ctime = now;
    }
}

/// chmod と chown(どのノードも同じ)
macro_rules! meta_ops {
    () => {
        fn as_any(&self) -> Option<&dyn core::any::Any> {
            Some(self)
        }

        fn set_mode(&self, mode: u32) -> Result<(), Errno> {
            let mut m = self.meta.lock();
            m.mode = mode & 0o7777;
            m.ctime = Timespec::now();
            Ok(())
        }

        fn set_owner(&self, uid: u32, gid: u32) -> Result<(), Errno> {
            let mut m = self.meta.lock();
            m.uid = uid;
            m.gid = gid;
            m.ctime = Timespec::now();
            Ok(())
        }

        fn set_times(&self, atime: Option<Timespec>, mtime: Option<Timespec>) -> Result<(), Errno> {
            let mut m = self.meta.lock();
            if let Some(t) = atime {
                m.atime = t;
            }
            if let Some(t) = mtime {
                m.mtime = t;
            }
            m.ctime = Timespec::now();
            Ok(())
        }
    };
}

struct TmpFile {
    data: SpinLock<Vec<u8>>,
    meta: SpinLock<Meta>,
    ino: u64,
}

impl File for TmpFile {
    meta_ops!();

    fn stat(&self) -> Stat {
        let size = self.data.lock().len() as u64;
        Meta::stat(&self.meta, FileKind::Regular, size, self.ino)
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        let data = self.data.lock();
        let start = (offset as usize).min(data.len());
        let n = buf.len().min(data.len() - start);
        buf[..n].copy_from_slice(&data[start..start + n]);
        Ok(n)
    }

    fn write(&self, offset: u64, buf: &[u8]) -> Result<usize, Errno> {
        let mut data = self.data.lock();
        let end = offset as usize + buf.len();
        if data.len() < end {
            data.resize(end, 0);
        }
        data[offset as usize..end].copy_from_slice(buf);
        Meta::modified(&self.meta);
        Ok(buf.len())
    }

    fn truncate(&self, len: u64) -> Result<(), Errno> {
        self.data.lock().resize(len as usize, 0);
        Meta::modified(&self.meta);
        Ok(())
    }
}

struct TmpDir {
    meta: SpinLock<Meta>,
    ino: u64,
}

impl File for TmpDir {
    meta_ops!();

    fn stat(&self) -> Stat {
        Meta::stat(&self.meta, FileKind::Directory, 0, self.ino)
    }

    fn read(&self, _offset: u64, _buf: &mut [u8]) -> Result<usize, Errno> {
        Err(Errno::EISDIR)
    }

    fn write(&self, _offset: u64, _buf: &[u8]) -> Result<usize, Errno> {
        Err(Errno::EISDIR)
    }
}

/// シンボリックリンク:指す先のパスを持つだけ
struct TmpLink {
    target: String,
    meta: SpinLock<Meta>,
    ino: u64,
}

impl File for TmpLink {
    meta_ops!();

    fn stat(&self) -> Stat {
        Meta::stat(
            &self.meta,
            FileKind::Symlink,
            self.target.len() as u64,
            self.ino,
        )
    }

    fn readlink(&self) -> Result<String, Errno> {
        Ok(self.target.clone())
    }
}

pub struct Tmpfs {
    nodes: SpinLock<BTreeMap<String, Arc<dyn File>>>,
    next_ino: AtomicU64,
}

/// 親ディレクトリのパス(`/a/b` → `/a`、`/a` → `/`)
fn parent(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(i) => &path[..i],
    }
}

/// tmpfs のノードの `Meta`(リンク数を数えるため)
fn meta_of(f: &Arc<dyn File>) -> Option<&SpinLock<Meta>> {
    let any = f.as_any()?;
    if let Some(x) = any.downcast_ref::<TmpFile>() {
        Some(&x.meta)
    } else if let Some(x) = any.downcast_ref::<TmpLink>() {
        Some(&x.meta)
    } else {
        any.downcast_ref::<TmpDir>().map(|x| &x.meta)
    }
}

/// 名前が 1 つ減った(ハードリンクの数を減らす)
fn drop_name(f: &Arc<dyn File>) {
    if let Some(m) = meta_of(f) {
        let mut m = m.lock();
        m.nlink = m.nlink.saturating_sub(1);
        m.ctime = Timespec::now();
    }
}

impl Tmpfs {
    pub fn new() -> Self {
        let mut nodes: BTreeMap<String, Arc<dyn File>> = BTreeMap::new();
        nodes.insert(
            String::from("/"),
            Arc::new(TmpDir {
                meta: Meta::new(0o1777),
                ino: 1,
            }),
        );
        Self {
            nodes: SpinLock::new(nodes),
            next_ino: AtomicU64::new(2),
        }
    }

    /// 新しい項目を足す前の確かめ:親がディレクトリで、同じ名前がないこと
    fn check_new(nodes: &BTreeMap<String, Arc<dyn File>>, path: &str) -> Result<(), Errno> {
        if nodes.contains_key(path) {
            return Err(Errno::EEXIST);
        }
        match nodes.get(parent(path)) {
            None => Err(Errno::ENOENT),
            Some(p) if p.stat().kind != FileKind::Directory => Err(Errno::ENOTDIR),
            Some(_) => Ok(()),
        }
    }
}

impl Default for Tmpfs {
    fn default() -> Self {
        Self::new()
    }
}

impl FileSystem for Tmpfs {
    fn lookup(&self, path: &str) -> Result<Arc<dyn File>, Errno> {
        self.nodes.lock().get(path).cloned().ok_or(Errno::ENOENT)
    }

    fn create(&self, path: &str, mode: u32) -> Result<Arc<dyn File>, Errno> {
        let mut nodes = self.nodes.lock();
        Self::check_new(&nodes, path)?;
        let file: Arc<dyn File> = Arc::new(TmpFile {
            data: SpinLock::new(Vec::new()),
            meta: Meta::new(mode),
            ino: self.next_ino.fetch_add(1, Ordering::Relaxed),
        });
        nodes.insert(String::from(path), file.clone());
        Ok(file)
    }

    fn mkdir(&self, path: &str, mode: u32) -> Result<(), Errno> {
        let mut nodes = self.nodes.lock();
        Self::check_new(&nodes, path)?;
        let ino = self.next_ino.fetch_add(1, Ordering::Relaxed);
        nodes.insert(
            String::from(path),
            Arc::new(TmpDir {
                meta: Meta::new(mode),
                ino,
            }),
        );
        Ok(())
    }

    fn unlink(&self, path: &str, dir: bool) -> Result<(), Errno> {
        let mut nodes = self.nodes.lock();
        let kind = nodes.get(path).ok_or(Errno::ENOENT)?.stat().kind;
        match (kind == FileKind::Directory, dir) {
            (true, false) => return Err(Errno::EISDIR),
            (false, true) => return Err(Errno::ENOTDIR),
            _ => {}
        }
        if dir {
            let prefix = alloc::format!("{path}/");
            if nodes.keys().any(|k| k.starts_with(&prefix)) {
                return Err(Errno::ENOTEMPTY);
            }
        }
        if let Some(f) = nodes.remove(path) {
            drop_name(&f);
        }
        Ok(())
    }

    fn link(&self, old: &str, new: &str) -> Result<(), Errno> {
        let mut nodes = self.nodes.lock();
        let file = nodes.get(old).cloned().ok_or(Errno::ENOENT)?;
        Self::check_new(&nodes, new)?;
        if let Some(m) = meta_of(&file) {
            let mut m = m.lock();
            m.nlink += 1;
            m.ctime = Timespec::now();
        }
        nodes.insert(String::from(new), file);
        Ok(())
    }

    fn symlink(&self, path: &str, target: &str) -> Result<(), Errno> {
        let mut nodes = self.nodes.lock();
        Self::check_new(&nodes, path)?;
        let ino = self.next_ino.fetch_add(1, Ordering::Relaxed);
        nodes.insert(
            String::from(path),
            Arc::new(TmpLink {
                target: String::from(target),
                meta: Meta::new(0o777),
                ino,
            }),
        );
        Ok(())
    }

    fn rename(&self, old: &str, new: &str) -> Result<(), Errno> {
        let mut nodes = self.nodes.lock();
        let kind = nodes.get(old).ok_or(Errno::ENOENT)?.stat().kind;
        match nodes.get(parent(new)) {
            None => return Err(Errno::ENOENT),
            Some(p) if p.stat().kind != FileKind::Directory => return Err(Errno::ENOTDIR),
            Some(_) => {}
        }
        let below = |dir: &str| alloc::format!("{dir}/");
        if let Some(target) = nodes.get(new) {
            let new_prefix = below(new);
            let empty = !nodes.keys().any(|k| k.starts_with(&new_prefix));
            crate::fs::vfs::check_replace(kind, target.stat().kind, empty)?;
            if let Some(f) = nodes.remove(new) {
                drop_name(&f);
            }
        }
        // 自分と、ディレクトリなら中身すべての名前を付け替える(パスが鍵なので)
        let old_prefix = below(old);
        let moved: Vec<String> = nodes
            .keys()
            .filter(|k| k.as_str() == old || k.starts_with(&old_prefix))
            .cloned()
            .collect();
        for key in moved {
            let file = nodes.remove(&key).expect("listed above");
            let new_key = alloc::format!("{new}{}", &key[old.len()..]);
            nodes.insert(new_key, file);
        }
        Ok(())
    }

    fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, Errno> {
        let nodes = self.nodes.lock();
        let dir = nodes.get(path).ok_or(Errno::ENOENT)?;
        if dir.stat().kind != FileKind::Directory {
            return Err(Errno::ENOTDIR);
        }
        Ok(nodes
            .iter()
            .filter(|(p, _)| p.as_str() != path && parent(p) == path)
            .map(|(p, f)| {
                let st = f.stat();
                DirEntry {
                    name: String::from(&p[p.rfind('/').map_or(0, |i| i + 1)..]),
                    kind: st.kind,
                    ino: st.ino,
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Errno;
    use crate::fs::vfs::{FileKind, FileSystem};

    #[test]
    fn created_files_can_be_written_and_read_back() {
        let fs = Tmpfs::new();
        let f = fs.create("/notes.txt", 0o644).unwrap();
        assert_eq!(f.write(0, b"hello"), Ok(5));
        assert_eq!(f.write(5, b", tmpfs"), Ok(7));
        let mut buf = [0u8; 32];
        assert_eq!(f.read(0, &mut buf), Ok(12));
        assert_eq!(&buf[..12], b"hello, tmpfs");
        assert_eq!(fs.lookup("/notes.txt").unwrap().stat().size, 12);
    }

    #[test]
    fn writing_past_the_end_fills_the_gap_with_zeros() {
        let fs = Tmpfs::new();
        let f = fs.create("/a", 0o644).unwrap();
        f.write(4, b"x").unwrap();
        let mut buf = [9u8; 5];
        assert_eq!(f.read(0, &mut buf), Ok(5));
        assert_eq!(buf, [0, 0, 0, 0, b'x']);
    }

    #[test]
    fn truncate_shrinks_and_grows() {
        let fs = Tmpfs::new();
        let f = fs.create("/a", 0o644).unwrap();
        f.write(0, b"abcdef").unwrap();
        f.truncate(2).unwrap();
        assert_eq!(f.stat().size, 2);
        f.truncate(4).unwrap();
        let mut buf = [9u8; 4];
        f.read(0, &mut buf).unwrap();
        assert_eq!(buf, [b'a', b'b', 0, 0]);
    }

    #[test]
    fn directories_hold_entries_and_are_listed() {
        let fs = Tmpfs::new();
        fs.mkdir("/d", 0o755).unwrap();
        fs.create("/d/one", 0o644).unwrap();
        fs.mkdir("/d/sub", 0o755).unwrap();
        assert_eq!(fs.lookup("/d").unwrap().stat().kind, FileKind::Directory);
        let names: std::vec::Vec<_> = fs
            .readdir("/d")
            .unwrap()
            .into_iter()
            .map(|e| (e.name, e.kind))
            .collect();
        assert_eq!(
            names,
            [
                ("one".into(), FileKind::Regular),
                ("sub".into(), FileKind::Directory)
            ]
        );
    }

    #[test]
    fn creating_errors() {
        let fs = Tmpfs::new();
        assert_eq!(fs.create("/missing/x", 0o644).err(), Some(Errno::ENOENT));
        fs.create("/f", 0o644).unwrap();
        assert_eq!(fs.mkdir("/f", 0o755).err(), Some(Errno::EEXIST));
        assert_eq!(fs.create("/f/x", 0o644).err(), Some(Errno::ENOTDIR));
    }

    #[test]
    fn unlink_removes_files_and_empty_directories_only() {
        let fs = Tmpfs::new();
        fs.mkdir("/d", 0o755).unwrap();
        fs.create("/d/f", 0o644).unwrap();
        assert_eq!(fs.unlink("/d", true), Err(Errno::ENOTEMPTY));
        assert_eq!(fs.unlink("/d", false), Err(Errno::EISDIR));
        assert_eq!(fs.unlink("/d/f", true), Err(Errno::ENOTDIR));
        fs.unlink("/d/f", false).unwrap();
        assert_eq!(fs.lookup("/d/f").err(), Some(Errno::ENOENT));
        fs.unlink("/d", true).unwrap();
        assert_eq!(fs.lookup("/d").err(), Some(Errno::ENOENT));
    }

    #[test]
    fn an_open_file_survives_unlink() {
        let fs = Tmpfs::new();
        let f = fs.create("/f", 0o644).unwrap();
        f.write(0, b"still here").unwrap();
        fs.unlink("/f", false).unwrap();
        let mut buf = [0u8; 10];
        assert_eq!(f.read(0, &mut buf), Ok(10));
    }
}
