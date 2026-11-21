//! tar(ustar 形式)の読み取り。initramfs(起動時にメモリに置くファイルの束)に使う。
//!
//! tar は「512 バイトのヘッダ + 中身(512 バイト単位に切り上げ)」の繰り返しで、
//! 最後に 0 で埋めたブロックが 2 つ続く。ヘッダの数値は 8 進数の文字列。

use crate::Errno;

const BLOCK: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
    Symlink,
    Other,
}

/// tar の1項目。
#[derive(Debug, Clone, Copy)]
pub struct Entry<'a> {
    /// 先頭の `./` と末尾の `/` を除いたパス
    pub path: &'a str,
    pub kind: Kind,
    pub mode: u32,
    pub data: &'a [u8],
    /// シンボリックリンクの指す先
    pub link: &'a str,
    /// 所有者
    pub uid: u32,
    pub gid: u32,
}

fn octal(field: &[u8]) -> Option<u64> {
    let mut v = 0u64;
    for &b in field {
        match b {
            b'0'..=b'7' => v = v * 8 + (b - b'0') as u64,
            b' ' | 0 => {
                if v > 0 || b == 0 {
                    break;
                }
            }
            _ => return None,
        }
    }
    Some(v)
}

fn cstr(field: &[u8]) -> &str {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    core::str::from_utf8(&field[..end]).unwrap_or("")
}

fn parse_header<'a>(tar: &'a [u8], at: usize) -> Result<Option<(Entry<'a>, usize)>, Errno> {
    let h = tar.get(at..at + BLOCK).ok_or(Errno::EIO)?;
    if h.iter().all(|&b| b == 0) {
        return Ok(None); // 終わりの印
    }
    // チェックサム:チェックサム欄を空白とみなした全バイトの和
    let stored = octal(&h[148..156]).ok_or(Errno::EIO)?;
    let sum: u64 = h
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            if (148..156).contains(&i) {
                b' ' as u64
            } else {
                b as u64
            }
        })
        .sum();
    if stored != sum {
        return Err(Errno::EIO);
    }
    let size = octal(&h[124..136]).ok_or(Errno::EIO)? as usize;
    let data = tar.get(at + BLOCK..at + BLOCK + size).ok_or(Errno::EIO)?;
    // 100 バイトを超える長いパスは prefix 欄(345〜)に分かれるが、initramfs では使わない
    let path = cstr(&h[0..100])
        .trim_start_matches("./")
        .trim_end_matches('/');
    let kind = match h[156] {
        b'0' | 0 => Kind::File,
        b'5' => Kind::Directory,
        b'2' => Kind::Symlink,
        _ => Kind::Other,
    };
    let entry = Entry {
        path,
        kind,
        mode: octal(&h[100..108]).ok_or(Errno::EIO)? as u32,
        data,
        link: cstr(&h[157..257]),
        uid: octal(&h[108..116]).ok_or(Errno::EIO)? as u32,
        gid: octal(&h[116..124]).ok_or(Errno::EIO)? as u32,
    };
    Ok(Some((entry, at + BLOCK + size.div_ceil(BLOCK) * BLOCK)))
}

/// tar の項目を順に返す。壊れていたら `Err(EIO)` を返して終わる。
pub fn entries(tar: &[u8]) -> impl Iterator<Item = Result<Entry<'_>, Errno>> {
    let mut at = 0;
    let mut done = false;
    core::iter::from_fn(move || {
        if done {
            return None;
        }
        match parse_header(tar, at) {
            Ok(Some((entry, next))) => {
                at = next;
                Some(Ok(entry))
            }
            Ok(None) => {
                done = true;
                None
            }
            Err(e) => {
                done = true;
                Some(Err(e))
            }
        }
    })
}

/// tar の中のファイル `path`(先頭の `/` は無視)の中身。
pub fn find<'a>(tar: &'a [u8], path: &str) -> Option<&'a [u8]> {
    let path = path.trim_start_matches('/');
    entries(tar)
        .map_while(Result::ok)
        .find(|e| e.kind == Kind::File && e.path == path)
        .map(|e| e.data)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::vec::Vec;

    /// テスト用の tar を作る。(名前, 種類, 中身)
    pub fn build(entries: &[(&str, u8, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, kind, data) in entries {
            let mut h = [0u8; 512];
            h[..name.len()].copy_from_slice(name.as_bytes());
            h[100..107].copy_from_slice(b"0000755");
            let size = std::format!("{:011o}", data.len());
            h[124..135].copy_from_slice(size.as_bytes());
            h[136..147].copy_from_slice(b"00000000000");
            h[156] = *kind;
            h[257..263].copy_from_slice(b"ustar\0");
            h[263..265].copy_from_slice(b"00");
            h[148..156].copy_from_slice(b"        ");
            let sum: u32 = h.iter().map(|&b| b as u32).sum();
            let sum = std::format!("{sum:06o}\0 ");
            h[148..156].copy_from_slice(sum.as_bytes());
            out.extend_from_slice(&h);
            out.extend_from_slice(data);
            out.resize(out.len().div_ceil(512) * 512, 0);
        }
        out.extend_from_slice(&[0; 1024]);
        out
    }

    /// テスト用:`name` の項目のモードを書き換え、チェックサムを直す。
    pub fn set_mode(tar: &mut [u8], name: &str, mode: u32) {
        let mut at = 0;
        while at + 512 <= tar.len() && tar[at] != 0 {
            let size = super::octal(&tar[at + 124..at + 136]).unwrap() as usize;
            if super::cstr(&tar[at..at + 100]) == name {
                let h = &mut tar[at..at + 512];
                h[100..107].copy_from_slice(std::format!("{mode:07o}").as_bytes());
                h[148..156].copy_from_slice(b"        ");
                let sum: u32 = h.iter().map(|&b| b as u32).sum();
                h[148..156].copy_from_slice(std::format!("{sum:06o}\0 ").as_bytes());
                return;
            }
            at += 512 + size.div_ceil(512) * 512;
        }
        panic!("{name} not in tar");
    }

    /// テスト用:`name` の項目の、シンボリックリンクの指す先を書く
    pub fn set_link(tar: &mut [u8], name: &str, link: &str) {
        let mut at = 0;
        while at + 512 <= tar.len() && tar[at] != 0 {
            let size = super::octal(&tar[at + 124..at + 136]).unwrap() as usize;
            if super::cstr(&tar[at..at + 100]) == name {
                let h = &mut tar[at..at + 512];
                h[157..157 + link.len()].copy_from_slice(link.as_bytes());
                h[148..156].copy_from_slice(b"        ");
                let sum: u32 = h.iter().map(|&b| b as u32).sum();
                h[148..156].copy_from_slice(std::format!("{sum:06o}\0 ").as_bytes());
                return;
            }
            at += 512 + size.div_ceil(512) * 512;
        }
        panic!("{name} not in tar");
    }

    #[test]
    fn owner_ids_are_read_from_the_header() {
        let mut tar = build(&[("a", b'0', b"x")]);
        tar[108..115].copy_from_slice(b"0001750"); // uid 1000
        tar[116..123].copy_from_slice(b"0000144"); // gid 100
        tar[148..156].copy_from_slice(b"        ");
        let sum: u32 = tar[..512].iter().map(|&b| b as u32).sum();
        tar[148..156].copy_from_slice(std::format!("{sum:06o}\0 ").as_bytes());
        let e = entries(&tar).next().unwrap().unwrap();
        assert_eq!((e.uid, e.gid), (1000, 100));
    }

    #[test]
    fn lists_files_and_directories_with_contents() {
        let tar = build(&[
            ("bin/", b'5', b""),
            ("bin/hello", b'0', b"\x7fELF..."),
            ("hello.js", b'0', b"console.log('hi')\n"),
        ]);
        let entries: Vec<_> = entries(&tar).collect::<Result<_, _>>().unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].path, "bin");
        assert_eq!(entries[0].kind, Kind::Directory);
        assert_eq!(entries[1].path, "bin/hello");
        assert_eq!(entries[1].kind, Kind::File);
        assert_eq!(entries[1].data, b"\x7fELF...");
        assert_eq!(entries[2].data, b"console.log('hi')\n");
        assert_eq!(entries[2].mode, 0o755);
    }

    #[test]
    fn leading_dot_slash_is_removed_from_paths() {
        let tar = build(&[("./etc/motd", b'0', b"hi")]);
        let e = entries(&tar).next().unwrap().unwrap();
        assert_eq!(e.path, "etc/motd");
    }

    #[test]
    fn find_returns_file_contents_by_absolute_path() {
        let tar = build(&[("bin/", b'5', b""), ("bin/hello", b'0', b"ELF")]);
        assert_eq!(find(&tar, "/bin/hello"), Some(&b"ELF"[..]));
        assert_eq!(find(&tar, "/bin"), None);
        assert_eq!(find(&tar, "/nope"), None);
    }

    #[test]
    fn corrupted_checksum_is_an_error() {
        let mut tar = build(&[("a", b'0', b"x")]);
        tar[0] = b'b';
        assert!(entries(&tar).next().unwrap().is_err());
    }

    #[test]
    fn truncated_archive_is_an_error() {
        let tar = build(&[("a", b'0', &[1u8; 1000])]);
        assert!(entries(&tar[..700]).any(|e| e.is_err()));
    }

    #[test]
    fn symlinks_carry_their_target() {
        let mut tar = build(&[("bin/js", b'2', b"")]);
        tar[157..160].copy_from_slice(b"qjs");
        // チェックサムを直す
        tar[148..156].copy_from_slice(b"        ");
        let sum: u32 = tar[..512].iter().map(|&b| b as u32).sum();
        tar[148..156].copy_from_slice(std::format!("{sum:06o}\0 ").as_bytes());
        let e = entries(&tar).next().unwrap().unwrap();
        assert_eq!(e.kind, Kind::Symlink);
        assert_eq!(e.link, "qjs");
    }
}
