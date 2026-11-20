//! procfs:カーネルの状態を、読めるファイルとして見せる(`/proc`)。
//!
//! 中身は、パスを引いたときにその時点の値で作る。Linux と同じ形のもの(`uptime`、`meminfo`、
//! `<pid>/status`)に加えて、Pi-OS 独自の観察用のファイルを `/proc/pios/` に置く。

use crate::Errno;
use crate::fs::vfs::{DirEntry, File, FileKind, FileSystem, Stat};
use crate::process::PROCESSES;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

/// その時点の中身を持つ読み出し専用のファイル
struct ProcFile {
    data: Vec<u8>,
}

impl File for ProcFile {
    fn stat(&self) -> Stat {
        Stat {
            kind: FileKind::Regular,
            mode: 0o444,
            size: 0, // Linux と同じく、大きさは 0 に見せる(読んでみるまで分からない)
            ino: 0x4_0000,
            uid: 0,
            gid: 0,
        }
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        let start = (offset as usize).min(self.data.len());
        let n = buf.len().min(self.data.len() - start);
        buf[..n].copy_from_slice(&self.data[start..start + n]);
        Ok(n)
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

struct ProcDir;

impl File for ProcDir {
    fn stat(&self) -> Stat {
        Stat {
            kind: FileKind::Directory,
            mode: 0o555,
            size: 0,
            ino: 0x4_0001,
            uid: 0,
            gid: 0,
        }
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

fn file(text: String) -> Result<Arc<dyn File>, Errno> {
    Ok(Arc::new(ProcFile {
        data: text.into_bytes(),
    }))
}

fn uptime() -> String {
    let ns = crate::time::now_ns();
    format!(
        "{}.{:02} 0.00\n",
        ns / 1_000_000_000,
        ns % 1_000_000_000 / 10_000_000
    )
}

fn meminfo() -> String {
    let (free, total) = crate::mm::frame_stats();
    let kb = |frames: usize| frames * 4;
    format!(
        "MemTotal:       {:8} kB\nMemFree:        {:8} kB\nHeapFree:       {:8} kB\n",
        kb(total),
        kb(free),
        crate::mm::HEAP.free_bytes() / 1024
    )
}

fn status(pid: crate::process::Pid, ppid: crate::process::Pid, name: &str) -> String {
    format!("Name:\t{name}\nState:\tR (running)\nPid:\t{pid}\nPPid:\t{ppid}\nUid:\t0\t0\t0\t0\n")
}

fn unimplemented() -> String {
    let mut text = String::from("# nr count\n");
    for (nr, count) in crate::syscall::unimplemented_list() {
        text.push_str(&format!("{nr} {count}\n"));
    }
    text
}

/// `/self` を今のプロセスの番号に置き換える
fn resolve_self(path: &str) -> String {
    match path.strip_prefix("/self") {
        Some(rest) => format!("/{}{rest}", crate::sched::current()),
        None => String::from(path),
    }
}

fn find(pid: crate::process::Pid) -> Option<(crate::process::Pid, String)> {
    PROCESSES
        .infos()
        .into_iter()
        .find(|(p, _, _)| *p == pid)
        .map(|(_, ppid, name)| (ppid, name))
}

#[derive(Default)]
pub struct Procfs;

impl Procfs {
    pub fn new() -> Self {
        Self
    }
}

fn entry(name: &str, kind: FileKind) -> DirEntry {
    DirEntry {
        name: String::from(name),
        kind,
        ino: 0x4_0002,
    }
}

impl FileSystem for Procfs {
    fn lookup(&self, path: &str) -> Result<Arc<dyn File>, Errno> {
        let path = resolve_self(path);
        match path.as_str() {
            "/" | "/pios" => Ok(Arc::new(ProcDir)),
            "/uptime" => file(uptime()),
            "/meminfo" => file(meminfo()),
            "/version" => file(String::from("Pi-OS version 0.1.0 (aarch64)\n")),
            "/pios/unimplemented" => file(unimplemented()),
            other => {
                let mut parts = other.trim_start_matches('/').splitn(2, '/');
                let pid: crate::process::Pid = parts
                    .next()
                    .and_then(|p| p.parse().ok())
                    .ok_or(Errno::ENOENT)?;
                let (ppid, name) = find(pid).ok_or(Errno::ENOENT)?;
                match parts.next() {
                    None => Ok(Arc::new(ProcDir)),
                    Some("status") => file(status(pid, ppid, &name)),
                    Some(_) => Err(Errno::ENOENT),
                }
            }
        }
    }

    fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, Errno> {
        let path = resolve_self(path);
        match path.as_str() {
            "/" => {
                let mut entries: Vec<DirEntry> = ["meminfo", "uptime", "version"]
                    .iter()
                    .map(|n| entry(n, FileKind::Regular))
                    .collect();
                entries.push(entry("pios", FileKind::Directory));
                entries.push(entry("self", FileKind::Directory));
                for (pid, _, _) in PROCESSES.infos() {
                    entries.push(entry(&format!("{pid}"), FileKind::Directory));
                }
                Ok(entries)
            }
            "/pios" => Ok(alloc::vec![entry("unimplemented", FileKind::Regular)]),
            other => {
                let pid: crate::process::Pid = other
                    .trim_start_matches('/')
                    .parse()
                    .map_err(|_| Errno::ENOTDIR)?;
                find(pid).ok_or(Errno::ENOENT)?;
                Ok(alloc::vec![entry("status", FileKind::Regular)])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Errno;
    use crate::fs::vfs::{FileKind, FileSystem};
    use crate::process::PROCESSES;
    use std::string::String;

    fn read_all(fs: &Procfs, path: &str) -> String {
        let f = fs.lookup(path).unwrap();
        let mut buf = [0u8; 4096];
        let n = f.read(0, &mut buf).unwrap();
        String::from_utf8(buf[..n].to_vec()).unwrap()
    }

    #[test]
    fn uptime_meminfo_and_version_use_linux_formats() {
        let fs = Procfs::new();
        let up = read_all(&fs, "/uptime");
        assert!(
            up.ends_with('\n') && up.split_whitespace().count() == 2,
            "{up:?}"
        );
        let mem = read_all(&fs, "/meminfo");
        assert!(
            mem.starts_with("MemTotal:") && mem.contains("MemFree:") && mem.contains(" kB\n"),
            "{mem}"
        );
        assert!(read_all(&fs, "/version").starts_with("Pi-OS version"));
    }

    #[test]
    fn each_process_has_a_status_file() {
        let fs = Procfs::new();
        PROCESSES.add_info(901, 900, "sh");
        let status = read_all(&fs, "/901/status");
        assert!(status.contains("Name:\tsh\n"), "{status}");
        assert!(
            status.contains("Pid:\t901\n") && status.contains("PPid:\t900\n"),
            "{status}"
        );
        assert_eq!(fs.lookup("/901").unwrap().stat().kind, FileKind::Directory);
        assert_eq!(fs.lookup("/99999/status").err(), Some(Errno::ENOENT));
    }

    #[test]
    fn readdir_lists_files_and_processes() {
        let fs = Procfs::new();
        PROCESSES.add_info(902, 1, "svcman");
        let names: std::vec::Vec<String> = fs
            .readdir("/")
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        for want in ["meminfo", "uptime", "version", "pios", "902"] {
            assert!(
                names.iter().any(|n| n == want),
                "{want} missing from {names:?}"
            );
        }
    }

    #[test]
    fn unimplemented_syscalls_are_listed() {
        let fs = Procfs::new();
        let text = read_all(&fs, "/pios/unimplemented");
        assert!(text.starts_with("# nr count\n"), "{text}");
    }
}
