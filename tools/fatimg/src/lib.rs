//! ホストで FAT32 のイメージを作る。
//!
//! 書式(ブートセクタ、FAT、ルートディレクトリ)は `mkfs.fat` に任せ、ファイルとディレクトリは
//! ここで書き込む。8.3 に収まらない名前には長い名前(LFN)のエントリを付ける。
//! できたイメージは `check` で `fsck.fat` に確かめさせられる。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

const ATTR_DIR: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
const ATTR_LFN: u8 = 0x0F;

const EOC: u32 = 0x0FFF_FFFF;

fn temp_path(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    std::env::temp_dir().join(format!(
        "pios-fatimg-{}-{}-{tag}.img",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

/// 大きさ `size_mib` MiB の FAT32 のイメージに `files`(パス, 中身)を入れて返す。
/// パスは `/` 区切り(先頭の `/` はなくてよい)。途中のディレクトリは自動で作る。
pub fn build(size_mib: u32, label: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    let path = temp_path("build");
    let _ = std::fs::remove_file(&path);
    let status = Command::new("mkfs.fat")
        .args(["-F", "32", "-s", "1", "-n", label, "-C"])
        .arg(&path)
        .arg((size_mib * 1024).to_string())
        .stdout(std::process::Stdio::null())
        .status()
        .expect("mkfs.fat is needed to build FAT32 images (dosfstools)");
    assert!(status.success(), "mkfs.fat failed");
    let img = std::fs::read(&path).expect("read the image");
    let _ = std::fs::remove_file(&path);
    let mut fs = Writer::new(img);
    let mut root = Dir::default();
    for (p, data) in files {
        root.insert(p.trim_start_matches('/'), data);
    }
    fs.write_dir(&root, fs.root_cluster, 0);
    fs.finish()
}

/// `fsck.fat -n` で確かめる。問題がなければ Ok、あれば出力を Err で返す。
pub fn check(img: &[u8]) -> Result<(), String> {
    let path = temp_path("check");
    std::fs::write(&path, img).expect("write the image");
    let out = Command::new("fsck.fat")
        .arg("-n")
        .arg(&path)
        .output()
        .expect("fsck.fat is needed (dosfstools)");
    let _ = std::fs::remove_file(&path);
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    if out.status.success() {
        Ok(())
    } else {
        Err(text)
    }
}

#[derive(Default)]
struct Dir<'a> {
    dirs: BTreeMap<String, Dir<'a>>,
    files: BTreeMap<String, &'a [u8]>,
}

impl<'a> Dir<'a> {
    fn insert(&mut self, path: &str, data: &'a [u8]) {
        match path.split_once('/') {
            Some((first, rest)) => self
                .dirs
                .entry(first.to_string())
                .or_default()
                .insert(rest, data),
            None if path.is_empty() => {}
            None => {
                self.files.insert(path.to_string(), data);
            }
        }
    }
}

struct Writer {
    img: Vec<u8>,
    bps: usize,
    cluster_size: usize,
    fat_start: usize,
    fat_size: usize,
    nfats: usize,
    data_start: usize,
    root_cluster: u32,
    fsinfo: usize,
    clusters: u32,
    next_free: u32,
}

impl Writer {
    fn new(img: Vec<u8>) -> Self {
        let u16_at = |o: usize| u16::from_le_bytes([img[o], img[o + 1]]) as usize;
        let u32_at = |o: usize| u32::from_le_bytes(img[o..o + 4].try_into().unwrap());
        let bps = u16_at(11);
        let spc = img[13] as usize;
        let rsvd = u16_at(14);
        let nfats = img[16] as usize;
        let total = u32_at(32) as usize;
        let fat_size = u32_at(36) as usize;
        let root_cluster = u32_at(44);
        let fsinfo = u16_at(48);
        let data_start = rsvd + nfats * fat_size;
        let clusters = ((total - data_start) / spc) as u32;
        Writer {
            bps,
            cluster_size: bps * spc,
            fat_start: rsvd * bps,
            fat_size: fat_size * bps,
            nfats,
            data_start: data_start * bps,
            root_cluster,
            fsinfo: fsinfo * bps,
            clusters,
            next_free: root_cluster + 1,
            img,
        }
    }

    fn set_fat(&mut self, cluster: u32, value: u32) {
        for i in 0..self.nfats {
            let o = self.fat_start + i * self.fat_size + cluster as usize * 4;
            self.img[o..o + 4].copy_from_slice(&value.to_le_bytes());
        }
    }

    /// `n` 個のクラスタを続けて確保し、鎖にする。最初のクラスタを返す(0 個なら 0)。
    fn alloc(&mut self, n: usize) -> u32 {
        if n == 0 {
            return 0;
        }
        let first = self.next_free;
        assert!(
            first as usize + n <= self.clusters as usize + 2,
            "image is full"
        );
        for i in 0..n as u32 {
            let c = first + i;
            let next = if i + 1 == n as u32 { EOC } else { c + 1 };
            self.set_fat(c, next);
        }
        self.next_free += n as u32;
        first
    }

    fn cluster_offset(&self, cluster: u32) -> usize {
        self.data_start + (cluster as usize - 2) * self.cluster_size
    }

    /// 鎖になったクラスタに `data` を書く(`first` から連続して確保してある前提)。
    fn write_chain(&mut self, first: u32, data: &[u8]) {
        let o = self.cluster_offset(first);
        self.img[o..o + data.len()].copy_from_slice(data);
    }

    fn write_dir(&mut self, dir: &Dir, cluster: u32, parent: u32) {
        let mut entries: Vec<[u8; 32]> = Vec::new();
        let is_root = cluster == self.root_cluster;
        if is_root {
            // mkfs.fat が置いたボリュームラベルのエントリを残す
            let o = self.cluster_offset(cluster);
            let mut label = [0u8; 32];
            label.copy_from_slice(&self.img[o..o + 32]);
            if label[11] == 0x08 {
                entries.push(label);
            }
        } else {
            entries.push(short_entry(b".          ", ATTR_DIR, cluster, 0));
            entries.push(short_entry(b"..         ", ATTR_DIR, parent, 0));
        }
        let mut used: Vec<[u8; 11]> = Vec::new();
        // 子のディレクトリは先にクラスタを決めておく(中身はあとで書く)
        let mut subdirs = Vec::new();
        for (name, sub) in &dir.dirs {
            let count = 2 + sub.dirs.len() * 3 + sub.files.len() * 3 + 1;
            let n = (count * 32).div_ceil(self.cluster_size);
            let c = self.alloc(n);
            entries.extend(named_entries(name, ATTR_DIR, c, 0, &mut used));
            subdirs.push((sub, c));
        }
        for (name, data) in &dir.files {
            let c = self.alloc(data.len().div_ceil(self.cluster_size));
            if c != 0 {
                self.write_chain(c, data);
            }
            entries.extend(named_entries(
                name,
                ATTR_ARCHIVE,
                c,
                data.len() as u32,
                &mut used,
            ));
        }
        let bytes: Vec<u8> = entries.concat();
        if is_root {
            // ルートは mkfs.fat が 1 クラスタ用意している。足りなければ伸ばす
            let need = bytes.len().div_ceil(self.cluster_size).max(1);
            if need > 1 {
                let more = self.alloc(need - 1);
                // ルートのクラスタの次に連続していないので、書く場所を分ける
                self.set_fat(cluster, more);
                let first = self.cluster_size.min(bytes.len());
                let o = self.cluster_offset(cluster);
                self.img[o..o + first].copy_from_slice(&bytes[..first]);
                self.write_chain(more, &bytes[first..]);
            } else {
                self.write_chain(cluster, &bytes);
            }
        } else {
            self.write_chain(cluster, &bytes);
        }
        let parent_for_children = if is_root { 0 } else { cluster };
        for (sub, c) in subdirs {
            self.write_dir(sub, c, parent_for_children);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        // FSInfo:空きクラスタの数と、次に探し始める場所
        let free = self.clusters + 2 - self.next_free;
        let o = self.fsinfo;
        self.img[o + 488..o + 492].copy_from_slice(&free.to_le_bytes());
        self.img[o + 492..o + 496].copy_from_slice(&self.next_free.to_le_bytes());
        let _ = self.bps;
        self.img
    }
}

fn short_entry(name: &[u8; 11], attr: u8, cluster: u32, size: u32) -> [u8; 32] {
    let mut e = [0u8; 32];
    e[..11].copy_from_slice(name);
    e[11] = attr;
    e[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
    e[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
    e[28..32].copy_from_slice(&size.to_le_bytes());
    // 日付:1980-01-01(0 だと fsck.fat が気にすることがある)
    let date: u16 = 1 << 5 | 1;
    e[16..18].copy_from_slice(&date.to_le_bytes());
    e[18..20].copy_from_slice(&date.to_le_bytes());
    e[24..26].copy_from_slice(&date.to_le_bytes());
    e
}

/// 名前がそのまま 8.3(大文字)で書けるなら、その 11 バイト
fn exact_short(name: &str) -> Option<[u8; 11]> {
    let (base, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    if base.is_empty() || !s_valid(base, 8) || !s_valid(ext, 3) {
        return None;
    }
    let mut out = [b' '; 11];
    out[..base.len()].copy_from_slice(base.as_bytes());
    out[8..8 + ext.len()].copy_from_slice(ext.as_bytes());
    Some(out)
}

fn s_valid(s: &str, max: usize) -> bool {
    s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b"_-~!#$%&".contains(&b))
}

/// 長い名前に付ける 8.3 の別名(`BASE~N.EXT`)
fn alias(name: &str, used: &[[u8; 11]]) -> [u8; 11] {
    let clean = |s: &str| -> Vec<u8> {
        s.bytes()
            .filter(|b| b.is_ascii_alphanumeric())
            .map(|b| b.to_ascii_uppercase())
            .collect()
    };
    let (base, ext) = match name.rsplit_once('.') {
        Some((b, e)) if !b.is_empty() => (clean(b), clean(e)),
        _ => (clean(name), Vec::new()),
    };
    for n in 1.. {
        let tail = format!("~{n}");
        let keep = base.len().min(8 - tail.len());
        let mut out = [b' '; 11];
        out[..keep].copy_from_slice(&base[..keep]);
        out[keep..keep + tail.len()].copy_from_slice(tail.as_bytes());
        let e = ext.len().min(3);
        out[8..8 + e].copy_from_slice(&ext[..e]);
        if !used.contains(&out) {
            return out;
        }
    }
    unreachable!()
}

fn checksum(short: &[u8; 11]) -> u8 {
    short
        .iter()
        .fold(0u8, |sum, &c| (sum >> 1 | (sum & 1) << 7).wrapping_add(c))
}

/// 名前のエントリ:必要なら LFN のエントリ(後ろの部分から)と、8.3 のエントリ
fn named_entries(
    name: &str,
    attr: u8,
    cluster: u32,
    size: u32,
    used: &mut Vec<[u8; 11]>,
) -> Vec<[u8; 32]> {
    if let Some(short) = exact_short(name) {
        used.push(short);
        return vec![short_entry(&short, attr, cluster, size)];
    }
    let short = alias(name, used);
    used.push(short);
    let sum = checksum(&short);
    let mut units: Vec<u16> = name.encode_utf16().collect();
    let parts = units.len().div_ceil(13);
    if units.len() % 13 != 0 {
        units.push(0);
    }
    units.resize(parts * 13, 0xFFFF);
    let mut out = Vec::new();
    for seq in (1..=parts).rev() {
        let chunk = &units[(seq - 1) * 13..seq * 13];
        let mut e = [0u8; 32];
        e[0] = seq as u8 | if seq == parts { 0x40 } else { 0 };
        e[11] = ATTR_LFN;
        e[13] = sum;
        let slots = (1..11)
            .step_by(2)
            .chain((14..26).step_by(2))
            .chain((28..32).step_by(2));
        for (slot, u) in slots.zip(chunk) {
            e[slot..slot + 2].copy_from_slice(&u.to_le_bytes());
        }
        out.push(e);
    }
    out.push(short_entry(&short, attr, cluster, size));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_built_image_passes_fsck() {
        let big = vec![0x5a; 3000];
        let img = build(
            34,
            "PIOSBOOT",
            &[
                ("CONFIG.TXT", b"arm_64bit=1\n"),
                ("kernel8.img", &big),
                ("overlays/a-long-overlay-name.dtbo", b"dtbo"),
                ("overlays/README", b""),
            ],
        );
        check(&img).unwrap();
    }

    #[test]
    fn long_names_get_an_alias_and_checksum() {
        let mut used = Vec::new();
        let e = named_entries("kernel8.img", ATTR_ARCHIVE, 3, 10, &mut used);
        assert_eq!(e.len(), 2);
        assert_eq!(&e[1][..11], b"KERNEL~1IMG");
        assert_eq!(e[0][0], 0x41);
        assert_eq!(e[0][13], checksum(b"KERNEL~1IMG"));
        assert_eq!(exact_short("CONFIG.TXT"), Some(*b"CONFIG  TXT"));
        assert_eq!(exact_short("config.txt"), None);
    }
}
