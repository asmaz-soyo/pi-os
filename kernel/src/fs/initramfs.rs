//! initramfs:tar の中身を、読み出し専用のファイルシステムとして見せる。

use crate::Errno;
use crate::fs::tar::{self, Kind};
use crate::fs::vfs::{DirEntry, File, FileKind, FileSystem, Stat};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;

struct RamFile {
    data: &'static [u8],
    mode: u32,
    ino: u64,
    uid: u32,
    gid: u32,
}

impl File for RamFile {
    fn stat(&self) -> Stat {
        Stat {
            kind: FileKind::Regular,
            mode: self.mode & 0o777,
            size: self.data.len() as u64,
            ino: self.ino,
            uid: self.uid,
            gid: self.gid,
        }
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        let start = (offset as usize).min(self.data.len());
        let n = buf.len().min(self.data.len() - start);
        buf[..n].copy_from_slice(&self.data[start..start + n]);
        Ok(n)
    }

    fn write(&self, _offset: u64, _buf: &[u8]) -> Result<usize, Errno> {
        Err(Errno::EROFS)
    }

    fn contents(&self) -> Option<&[u8]> {
        Some(self.data)
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

struct RamDir {
    ino: u64,
}

impl File for RamDir {
    fn stat(&self) -> Stat {
        Stat {
            kind: FileKind::Directory,
            mode: 0o755,
            size: 0,
            ino: self.ino,
            uid: 0,
            gid: 0,
        }
    }

    fn read(&self, _offset: u64, _buf: &mut [u8]) -> Result<usize, Errno> {
        Err(Errno::EISDIR)
    }

    fn write(&self, _offset: u64, _buf: &[u8]) -> Result<usize, Errno> {
        Err(Errno::EISDIR)
    }
}

pub struct Initramfs {
    nodes: BTreeMap<String, Arc<dyn File>>,
}

impl Initramfs {
    pub fn new(tar_image: &'static [u8]) -> Result<Self, Errno> {
        let mut nodes: BTreeMap<String, Arc<dyn File>> = BTreeMap::new();
        let mut ino = 1;
        let mut next_ino = || {
            ino += 1;
            ino
        };
        nodes.insert(String::from("/"), Arc::new(RamDir { ino: 1 }));
        for entry in tar::entries(tar_image) {
            let entry = entry?;
            let path = alloc::format!("/{}", entry.path);
            // 親ディレクトリは tar に項目がなくても作る
            let mut parent = path.as_str();
            while let Some(i) = parent.rfind('/') {
                parent = &path[..i];
                if parent.is_empty() {
                    break;
                }
                if !nodes.contains_key(parent) {
                    nodes.insert(String::from(parent), Arc::new(RamDir { ino: next_ino() }));
                }
            }
            let node: Arc<dyn File> = match entry.kind {
                Kind::Directory => Arc::new(RamDir { ino: next_ino() }),
                Kind::File => Arc::new(RamFile {
                    data: entry.data,
                    mode: entry.mode,
                    ino: next_ino(),
                    uid: entry.uid,
                    gid: entry.gid,
                }),
                _ => continue,
            };
            nodes.insert(path, node);
        }
        Ok(Self { nodes })
    }
}

impl FileSystem for Initramfs {
    fn lookup(&self, path: &str) -> Result<Arc<dyn File>, Errno> {
        self.nodes.get(path).cloned().ok_or(Errno::ENOENT)
    }

    fn readdir(&self, path: &str) -> Result<alloc::vec::Vec<DirEntry>, Errno> {
        let dir = self.nodes.get(path).ok_or(Errno::ENOENT)?;
        if dir.stat().kind != FileKind::Directory {
            return Err(Errno::ENOTDIR);
        }
        let prefix = if path == "/" {
            String::from("/")
        } else {
            alloc::format!("{path}/")
        };
        Ok(self
            .nodes
            .iter()
            .filter_map(|(p, f)| {
                let name = p.strip_prefix(prefix.as_str())?;
                (!name.is_empty() && !name.contains('/')).then(|| {
                    let st = f.stat();
                    DirEntry {
                        name: String::from(name),
                        kind: st.kind,
                        ino: st.ino,
                    }
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Errno;
    use crate::fs::tar::tests::build;
    use crate::fs::vfs::FileKind;
    use alloc::boxed::Box;

    fn fs() -> Initramfs {
        let tar = build(&[
            ("bin/hello", b'0', b"ELF"),
            ("etc/motd", b'0', b"Welcome\n"),
        ]);
        Initramfs::new(Box::leak(tar.into_boxed_slice())).unwrap()
    }

    #[test]
    fn files_are_found_by_path_and_readable_at_offsets() {
        let fs = fs();
        let motd = fs.lookup("/etc/motd").unwrap();
        let mut buf = [0u8; 4];
        assert_eq!(motd.read(3, &mut buf), Ok(4));
        assert_eq!(&buf, b"come");
        assert_eq!(motd.read(8, &mut buf), Ok(0));
        assert_eq!(motd.stat().size, 8);
        assert_eq!(motd.stat().kind, FileKind::Regular);
    }

    #[test]
    fn parent_directories_exist_implicitly() {
        let fs = fs();
        assert_eq!(fs.lookup("/etc").unwrap().stat().kind, FileKind::Directory);
        assert_eq!(fs.lookup("/").unwrap().stat().kind, FileKind::Directory);
        let mut buf = [0u8; 1];
        assert_eq!(
            fs.lookup("/etc").unwrap().read(0, &mut buf),
            Err(Errno::EISDIR)
        );
    }

    #[test]
    fn missing_paths_are_enoent_and_files_are_read_only() {
        let fs = fs();
        assert_eq!(fs.lookup("/nope").err(), Some(Errno::ENOENT));
        assert_eq!(
            fs.lookup("/etc/motd").unwrap().write(0, b"x"),
            Err(Errno::EROFS)
        );
    }

    #[test]
    fn file_contents_are_available_for_mmap() {
        let fs = fs();
        assert_eq!(
            fs.lookup("/bin/hello").unwrap().contents(),
            Some(&b"ELF"[..])
        );
    }
}
