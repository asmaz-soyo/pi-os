//! procfs:カーネルの状態を、読めるファイルとして見せる(`/proc`)。
//!
//! 中身は、パスを引いたときにその時点の値で作る。Linux と同じ形のもの(`uptime`、`meminfo`、
//! `<pid>/status`)に加えて、Pi-OS 独自の観察用のファイルを `/proc/pios/` に置く。
//! `/proc/sys/net/pios/` の下は書ける(設定を変える)。

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
            ..Default::default()
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
            ..Default::default()
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

/// /proc/mounts:「デバイス マウント先 種類 オプション 0 0」
fn mounts() -> String {
    let mut text = String::new();
    for (source, target, kind) in crate::fs::mounts() {
        text.push_str(&format!("{source} {target} {kind} rw 0 0\n"));
    }
    text
}

/// /proc/interrupts:登録した割り込みの、番号・届いた回数・名前(Linux と同じ並び。CPU は 1 つ)
fn interrupts(counts: &[(u32, &str, usize)]) -> String {
    let mut text = String::from("           CPU0\n");
    for (irq, name, count) in counts {
        text.push_str(&format!("{irq:>4}: {count:>10}  {name}\n"));
    }
    text
}

/// /proc/pios/resolv.conf:DHCP でもらった DNS サーバー(`/etc/resolv.conf` がここを指す)
fn resolv_conf(servers: &[crate::net::inet::Ipv4Address]) -> String {
    let mut text = String::new();
    for s in servers {
        text.push_str(&format!("nameserver {s}\n"));
    }
    text
}

/// `/<pid>/status`。State は Linux と同じく、眠っていれば S、動いている(動ける)なら R
fn status(
    pid: crate::process::Pid,
    ppid: crate::process::Pid,
    name: &str,
    sleeping: bool,
) -> String {
    let state = if sleeping {
        "S (sleeping)"
    } else {
        "R (running)"
    };
    format!("Name:\t{name}\nState:\t{state}\nPid:\t{pid}\nPPid:\t{ppid}\nUid:\t0\t0\t0\t0\n")
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

/// 自作の TCP の一覧(`/pios/tcp`)
pub fn tcp_table(
    rows: &[(
        crate::net::tcp::conn::Endpoint,
        Option<crate::net::tcp::conn::Endpoint>,
        &str,
        u64,
    )],
) -> String {
    let mut text = format!(
        "{:<23}{:<23}{:<13}{}\n",
        "local", "remote", "state", "rto_ms"
    );
    for (local, remote, state, rto) in rows {
        let local = format!("{}:{}", local.addr, local.port);
        let remote = remote.map_or(String::from("-"), |r| format!("{}:{}", r.addr, r.port));
        let rto = if remote == "-" {
            String::from("-")
        } else {
            format!("{rto}")
        };
        text += &format!("{local:<23}{remote:<23}{state:<13}{rto}\n");
    }
    text
}

/// 読むと今の値、書くと設定が変わるファイル(Linux の `/proc/sys` と同じ)
struct SysFile {
    get: fn() -> &'static str,
    set: fn(&[u8]) -> Result<(), Errno>,
}

impl File for SysFile {
    fn stat(&self) -> Stat {
        Stat {
            kind: FileKind::Regular,
            mode: 0o644,
            size: 0,
            ino: 0x4_0003,
            uid: 0,
            gid: 0,
            ..Default::default()
        }
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        ProcFile {
            data: format!("{}\n", (self.get)()).into_bytes(),
        }
        .read(offset, buf)
    }

    fn write(&self, _offset: u64, buf: &[u8]) -> Result<usize, Errno> {
        (self.set)(buf)?;
        Ok(buf.len())
    }

    /// `echo x > file` は O_TRUNC で開くので、切り詰めは何もせずに受ける
    fn truncate(&self, _len: u64) -> Result<(), Errno> {
        Ok(())
    }
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
    /// 種類:proc
    fn statfs(&self) -> Result<crate::fs::vfs::StatFs, crate::Errno> {
        Ok(crate::fs::vfs::StatFs::of_kind(0x9fa0))
    }

    fn lookup(&self, path: &str) -> Result<Arc<dyn File>, Errno> {
        let path = resolve_self(path);
        match path.as_str() {
            "/" | "/pios" | "/pios/net" | "/sys" | "/sys/net" | "/sys/net/pios" => {
                Ok(Arc::new(ProcDir))
            }
            "/sys/net/pios/tcp" => Ok(Arc::new(SysFile {
                get: crate::net::inet::default_tcp,
                set: crate::net::inet::set_default_tcp,
            })),
            "/pios/tcp" => file(tcp_table(&crate::net::inet::own_tcp_table())),
            "/uptime" => file(uptime()),
            "/meminfo" => file(meminfo()),
            "/mounts" => file(mounts()),
            "/interrupts" => file(interrupts(&crate::irq::IRQS.counts())),
            "/version" => file(String::from("Pi-OS version 0.1.0 (aarch64)\n")),
            "/pios/unimplemented" => file(unimplemented()),
            "/pios/resolv.conf" => file(resolv_conf(&crate::net::inet::dns_servers())),
            other if other.starts_with("/pios/net/") => {
                let name = other["/pios/net/".len()..]
                    .strip_suffix(".pcap")
                    .ok_or(Errno::ENOENT)?;
                let pcap = crate::net::inet::pcap_of(name).ok_or(Errno::ENOENT)?;
                Ok(Arc::new(ProcFile { data: pcap }))
            }
            other => {
                let mut parts = other.trim_start_matches('/').splitn(2, '/');
                let pid: crate::process::Pid = parts
                    .next()
                    .and_then(|p| p.parse().ok())
                    .ok_or(Errno::ENOENT)?;
                let (ppid, name) = find(pid).ok_or(Errno::ENOENT)?;
                match parts.next() {
                    None => Ok(Arc::new(ProcDir)),
                    Some("status") => {
                        file(status(pid, ppid, &name, crate::sched::is_sleeping(pid)))
                    }
                    Some(_) => Err(Errno::ENOENT),
                }
            }
        }
    }

    fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, Errno> {
        let path = resolve_self(path);
        match path.as_str() {
            "/" => {
                let mut entries: Vec<DirEntry> =
                    ["interrupts", "meminfo", "mounts", "uptime", "version"]
                        .iter()
                        .map(|n| entry(n, FileKind::Regular))
                        .collect();
                entries.push(entry("pios", FileKind::Directory));
                entries.push(entry("self", FileKind::Directory));
                entries.push(entry("sys", FileKind::Directory));
                for (pid, _, _) in PROCESSES.infos() {
                    entries.push(entry(&format!("{pid}"), FileKind::Directory));
                }
                Ok(entries)
            }
            "/pios/net" => Ok(crate::net::inet::interface_names()
                .iter()
                .map(|n| entry(&format!("{n}.pcap"), FileKind::Regular))
                .collect()),
            "/pios" => Ok(alloc::vec![
                entry("net", FileKind::Directory),
                entry("resolv.conf", FileKind::Regular),
                entry("tcp", FileKind::Regular),
                entry("unimplemented", FileKind::Regular),
            ]),
            "/sys" => Ok(alloc::vec![entry("net", FileKind::Directory)]),
            "/sys/net" => Ok(alloc::vec![entry("pios", FileKind::Directory)]),
            "/sys/net/pios" => Ok(alloc::vec![entry("tcp", FileKind::Regular)]),
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
    fn mounts_lists_what_is_mounted_in_linux_format() {
        crate::fs::record_mount("/dev/vda2", "/", "ext3");
        crate::fs::record_mount("proc", "/proc", "proc");
        let fs = Procfs::new();
        let text = read_all(&fs, "/mounts");
        // 「デバイス マウント先 種類 オプション 0 0」
        assert!(text.contains("/dev/vda2 / ext3 rw 0 0\n"), "{text}");
        assert!(text.contains("proc /proc proc rw 0 0\n"), "{text}");
        assert!(fs.readdir("/").unwrap().iter().any(|e| e.name == "mounts"));
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
    fn resolv_conf_lists_one_nameserver_per_line() {
        use crate::net::inet::Ipv4Address;
        let text = resolv_conf(&[Ipv4Address::new(10, 0, 2, 3), Ipv4Address::new(1, 1, 1, 1)]);
        assert_eq!(text, "nameserver 10.0.2.3\nnameserver 1.1.1.1\n");
        // DHCP の前(サーバーがない)は空のファイル(musl は 127.0.0.1 に聞きに行く)
        assert_eq!(resolv_conf(&[]), "");
        let fs = Procfs::new();
        assert!(fs.lookup("/pios/resolv.conf").is_ok());
        assert!(
            fs.readdir("/pios")
                .unwrap()
                .iter()
                .any(|e| e.name == "resolv.conf")
        );
    }

    #[test]
    fn each_interface_has_a_pcap_file() {
        let fs = Procfs::new();
        let names: std::vec::Vec<String> = fs
            .readdir("/pios/net")
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert!(names.iter().any(|n| n == "lo.pcap"), "{names:?}");
        assert!(fs.readdir("/pios").unwrap().iter().any(|e| e.name == "net"));
        let f = fs.lookup("/pios/net/lo.pcap").unwrap();
        let mut head = [0u8; 24];
        assert_eq!(f.read(0, &mut head).unwrap(), 24);
        // pcap の目印と、リンクの種類(ループバックは 101 = IP のパケットそのもの)
        assert_eq!(&head[..4], &0xa1b2_c3d4u32.to_le_bytes());
        assert_eq!(&head[20..24], &101u32.to_le_bytes());
        assert_eq!(fs.lookup("/pios/net/nope.pcap").err(), Some(Errno::ENOENT));
    }

    #[test]
    fn interrupts_lists_counts_in_linux_format() {
        let text = interrupts(&[(30, "timer", 1234), (48, "virtio-net", 7)]);
        // 見出しは CPU ごとの列(今は CPU0 だけ)。各行は「番号: 回数 名前」
        assert_eq!(
            text,
            "           CPU0\n  30:       1234  timer\n  48:          7  virtio-net\n"
        );
        let fs = Procfs::new();
        assert!(read_all(&fs, "/interrupts").starts_with("           CPU0\n"));
        assert!(
            fs.readdir("/")
                .unwrap()
                .iter()
                .any(|e| e.name == "interrupts")
        );
    }

    #[test]
    fn unimplemented_syscalls_are_listed() {
        let fs = Procfs::new();
        let text = read_all(&fs, "/pios/unimplemented");
        assert!(text.starts_with("# nr count\n"), "{text}");
    }

    #[test]
    fn the_default_tcp_can_be_read_and_switched_through_sys() {
        let fs = Procfs::new();
        // 既定は smoltcp。ほかのテストと共有なので、ここでは今と同じ値を書くことと、だめな値だけを試す
        assert_eq!(read_all(&fs, "/sys/net/pios/tcp"), "smoltcp\n");
        let f = fs.lookup("/sys/net/pios/tcp").unwrap();
        assert!(!f.is_read_only());
        assert_eq!(f.stat().mode, 0o644);
        assert_eq!(f.write(0, b"smoltcp\n"), Ok(8));
        assert_eq!(f.write(0, b"lwip\n"), Err(Errno::EINVAL));
        // シェルの `echo own > ...` は O_TRUNC で開く
        assert_eq!(f.truncate(0), Ok(()));
        assert_eq!(read_all(&fs, "/sys/net/pios/tcp"), "smoltcp\n");
        for (dir, want) in [
            ("/", "sys"),
            ("/sys", "net"),
            ("/sys/net", "pios"),
            ("/sys/net/pios", "tcp"),
        ] {
            assert!(
                fs.readdir(dir).unwrap().iter().any(|e| e.name == want),
                "{dir}"
            );
        }
    }

    #[test]
    fn own_tcp_connections_are_listed() {
        use crate::net::tcp::conn::Endpoint;
        use core::net::Ipv4Addr;
        let any = Endpoint {
            addr: Ipv4Addr::UNSPECIFIED,
            port: 7300,
        };
        let me = Endpoint {
            addr: Ipv4Addr::new(127, 0, 0, 1),
            port: 49152,
        };
        let peer = Endpoint {
            addr: Ipv4Addr::new(127, 0, 0, 1),
            port: 7300,
        };
        let text = tcp_table(&[
            (any, None, "LISTEN", 0),
            (me, Some(peer), "ESTABLISHED", 200),
        ]);
        assert_eq!(
            text,
            "local                  remote                 state        rto_ms\n\
             0.0.0.0:7300           -                      LISTEN       -\n\
             127.0.0.1:49152        127.0.0.1:7300         ESTABLISHED  200\n"
        );
        let fs = Procfs::new();
        assert!(read_all(&fs, "/pios/tcp").starts_with("local "));
        assert!(fs.readdir("/pios").unwrap().iter().any(|e| e.name == "tcp"));
    }

    #[test]
    fn status_shows_whether_the_task_is_sleeping() {
        assert!(status(5, 1, "cat", true).contains("State:\tS (sleeping)\n"));
        assert!(status(5, 1, "cat", false).contains("State:\tR (running)\n"));
    }
}
