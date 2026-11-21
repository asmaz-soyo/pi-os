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
pub const O_EXCL: u32 = 0o200;
pub const O_TRUNC: u32 = 0o1000;
/// AArch64 の値(x86 の 0o200000 とは違う。x86 の値を使っていたのを互換テストで見つけた)
pub const O_DIRECTORY: u32 = 0o40000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FileKind {
    #[default]
    Regular,
    Directory,
    CharDevice,
    /// パイプ
    Fifo,
    /// シンボリックリンク(中身は別のパス)
    Symlink,
}

/// ファイルシステム全体の情報(statfs)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatFs {
    /// 種類(Linux と同じマジック番号:ext2 は 0xEF53 など)
    pub kind: u64,
    pub block_size: u64,
    /// ブロックの数:全体、空き、一般ユーザーが使える空き
    pub blocks: u64,
    pub free_blocks: u64,
    pub avail_blocks: u64,
    /// inode(ファイル)の数:全体、空き
    pub files: u64,
    pub free_files: u64,
    /// ファイル名の最大の長さ
    pub name_max: u64,
}

impl StatFs {
    /// 大きさを数えないファイルシステム(メモリの上のものなど)
    pub const fn of_kind(kind: u64) -> Self {
        Self {
            kind,
            block_size: 4096,
            blocks: 0,
            free_blocks: 0,
            avail_blocks: 0,
            files: 0,
            free_files: 0,
            name_max: 255,
        }
    }
}

/// 時刻(1970 年からの秒とナノ秒)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Timespec {
    pub sec: i64,
    pub nsec: u32,
}

impl Timespec {
    /// 今の時刻(CLOCK_REALTIME と同じ:起動時の時刻 + 起動からの時間)
    pub fn now() -> Self {
        let ns = crate::time::now_ns();
        Self {
            sec: (crate::time::BOOT_EPOCH + ns / 1_000_000_000) as i64,
            nsec: (ns % 1_000_000_000) as u32,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stat {
    pub kind: FileKind,
    /// パーミッション(下位 9 ビット)
    pub mode: u32,
    pub size: u64,
    pub ino: u64,
    /// 所有者
    pub uid: u32,
    pub gid: u32,
    /// ハードリンクの数(名前がいくつあるか)。0 は「数えていない」で、1 として見せる
    pub nlink: u32,
    /// 最後に読んだ時刻、中身を変えた時刻、属性(権限や持ち主)を変えた時刻
    pub atime: Timespec,
    pub mtime: Timespec,
    pub ctime: Timespec,
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

    /// 今読んでも待たずに済むか(select / poll 用)。普通のファイルは常に読める
    fn poll_readable(&self) -> bool {
        true
    }

    /// 端末の設定(端末だけ)
    fn termios(&self) -> Option<crate::tty::Termios> {
        None
    }

    fn set_termios(&self, _t: crate::tty::Termios) -> Result<(), Errno> {
        Err(Errno::ENOTTY)
    }

    /// 大きさを変える(書けるファイルだけ)
    fn truncate(&self, _len: u64) -> Result<(), Errno> {
        Err(Errno::EINVAL)
    }

    /// 書き込めないファイルシステムのファイルか(open で書き込みを求められたら EROFS にする)
    fn is_read_only(&self) -> bool {
        false
    }

    /// 権限のビット(下の 12 ビット:rwx と setuid・setgid・スティッキー)を変える
    fn set_mode(&self, _mode: u32) -> Result<(), Errno> {
        Err(Errno::EROFS)
    }

    /// 持ち主を変える
    fn set_owner(&self, _uid: u32, _gid: u32) -> Result<(), Errno> {
        Err(Errno::EROFS)
    }

    /// 最後に読んだ時刻と、中身を変えた時刻を設定する(None は変えない)
    fn set_times(&self, _atime: Option<Timespec>, _mtime: Option<Timespec>) -> Result<(), Errno> {
        Err(Errno::EROFS)
    }

    /// シンボリックリンクなら、指す先のパス。リンクでなければ EINVAL
    fn readlink(&self) -> Result<String, Errno> {
        Err(Errno::EINVAL)
    }

    /// 具体的な型として取り出す(timerfd や epoll の操作で使う)
    fn as_any(&self) -> Option<&dyn core::any::Any> {
        None
    }
}

/// ディレクトリの中の1項目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub kind: FileKind,
    pub ino: u64,
}

/// 開いたファイル。読み書きの位置とフラグを持つ。
pub struct OpenFile {
    pub file: Arc<dyn File>,
    pub flags: u32,
    pub offset: SpinLock<u64>,
    /// 開いたときの絶対パス(ディレクトリの一覧に使う)
    pub path: Option<String>,
    /// 追記(O_APPEND):書くときは毎回ファイルの終わりから。fcntl(F_SETFL)で変えられる
    pub append: core::sync::atomic::AtomicBool,
}

/// 追記で開く(書くときは毎回ファイルの終わりから)
pub const O_APPEND: u32 = 0o2000;

impl OpenFile {
    pub fn new(file: Arc<dyn File>, flags: u32) -> Arc<Self> {
        Arc::new(Self {
            file,
            flags,
            offset: SpinLock::new(0),
            path: None,
            append: core::sync::atomic::AtomicBool::new(flags & O_APPEND != 0),
        })
    }

    pub fn with_path(file: Arc<dyn File>, flags: u32, path: String) -> Arc<Self> {
        Arc::new(Self {
            file,
            flags,
            offset: SpinLock::new(0),
            path: Some(path),
            append: core::sync::atomic::AtomicBool::new(flags & O_APPEND != 0),
        })
    }

    /// 今の状態のフラグ(fcntl の F_GETFL)
    pub fn status_flags(&self) -> u32 {
        let append = self.append.load(core::sync::atomic::Ordering::Relaxed);
        (self.flags & !O_APPEND) | if append { O_APPEND } else { 0 }
    }

    pub fn readable(&self) -> bool {
        self.flags & O_ACCMODE != O_WRONLY
    }

    pub fn writable(&self) -> bool {
        self.flags & O_ACCMODE != O_RDONLY
    }
}

/// ファイル記述子の表。複製すると(fork)、開いたファイルは共有される。
#[derive(Clone)]
pub struct FdTable {
    fds: Vec<Option<Arc<OpenFile>>>,
    /// close-on-exec の印が付いた番号(execve で閉じる)
    cloexec: alloc::collections::BTreeSet<usize>,
}

/// 1つのプロセスが開けるファイルの数
pub const MAX_FDS: usize = 256;

impl FdTable {
    pub const fn new() -> Self {
        Self {
            fds: Vec::new(),
            cloexec: alloc::collections::BTreeSet::new(),
        }
    }

    /// 空いている一番小さい番号に入れる。
    pub fn insert(&mut self, file: Arc<OpenFile>) -> Result<usize, Errno> {
        self.insert_from(0, file)
    }

    /// `min` 以上で空いている一番小さい番号に入れる。
    pub fn insert_from(&mut self, min: usize, file: Arc<OpenFile>) -> Result<usize, Errno> {
        let fd = (min..MAX_FDS)
            .find(|&i| self.fds.get(i).is_none_or(Option::is_none))
            .ok_or(Errno::EMFILE)?;
        self.insert_at(fd, file)?;
        Ok(fd)
    }

    /// 番号 `fd` に入れる(開いていれば閉じてから)。
    pub fn insert_at(&mut self, fd: usize, file: Arc<OpenFile>) -> Result<(), Errno> {
        if fd >= MAX_FDS {
            return Err(Errno::EBADF);
        }
        if self.fds.len() <= fd {
            self.fds.resize(fd + 1, None);
        }
        self.fds[fd] = Some(file);
        self.cloexec.remove(&fd);
        Ok(())
    }

    pub fn get(&self, fd: u64) -> Result<Arc<OpenFile>, Errno> {
        self.fds
            .get(fd as usize)
            .and_then(Clone::clone)
            .ok_or(Errno::EBADF)
    }

    pub fn close(&mut self, fd: u64) -> Result<(), Errno> {
        let slot = self.fds.get_mut(fd as usize).ok_or(Errno::EBADF)?;
        self.cloexec.remove(&(fd as usize));
        slot.take().map(|_| ()).ok_or(Errno::EBADF)
    }

    pub fn set_cloexec(&mut self, fd: u64, on: bool) -> Result<(), Errno> {
        self.get(fd)?;
        if on {
            self.cloexec.insert(fd as usize);
        } else {
            self.cloexec.remove(&(fd as usize));
        }
        Ok(())
    }

    pub fn is_cloexec(&self, fd: u64) -> bool {
        self.cloexec.contains(&(fd as usize))
    }

    /// close-on-exec の印が付いたものを閉じる(execve の成功時)。
    pub fn close_on_exec(&mut self) {
        for fd in core::mem::take(&mut self.cloexec) {
            if let Some(slot) = self.fds.get_mut(fd) {
                *slot = None;
            }
        }
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

    /// ファイルシステム全体の情報(statfs)
    fn statfs(&self) -> Result<StatFs, Errno> {
        Ok(StatFs::of_kind(0))
    }

    /// まだ本来の場所に書いていないもの(ジャーナルにコミットしただけのもの)を書く
    fn sync(&self) -> Result<(), Errno> {
        Ok(())
    }

    /// 見つけた名前を VFS の dentry キャッシュに覚えてよいか。名前が VFS を通らずに変わる
    /// ファイルシステム(/proc のプロセス、/dev のデバイス)は false
    fn cacheable(&self) -> bool {
        false
    }

    /// ディレクトリ `dir`(このファイルシステムの中のパス `dir_path`)の中の `name` を探す。
    /// ファイルシステムは、`dir` の実体から探せばルートからたどり直さずに済む
    fn lookup_child(
        &self,
        _dir: &Arc<dyn File>,
        dir_path: &str,
        name: &str,
    ) -> Result<Arc<dyn File>, Errno> {
        self.lookup(&normalize(dir_path, name))
    }

    /// 新しいファイルを作る
    fn create(&self, _path: &str, _mode: u32) -> Result<Arc<dyn File>, Errno> {
        Err(Errno::EROFS)
    }

    fn mkdir(&self, _path: &str, _mode: u32) -> Result<(), Errno> {
        Err(Errno::EROFS)
    }

    /// ファイル(`dir` なら空のディレクトリ)を消す
    fn unlink(&self, _path: &str, _dir: bool) -> Result<(), Errno> {
        Err(Errno::EROFS)
    }

    /// ディレクトリの中身(名前の順)
    fn readdir(&self, _path: &str) -> Result<Vec<DirEntry>, Errno> {
        Err(Errno::ENOTDIR)
    }

    /// 特別なファイル(今は名前付きパイプ:`FileKind::Fifo` だけ)を作る
    fn mknod(&self, _path: &str, _kind: FileKind, _mode: u32) -> Result<(), Errno> {
        Err(Errno::EROFS)
    }

    /// `path` に、`target` を指すシンボリックリンクを作る
    fn symlink(&self, _path: &str, _target: &str) -> Result<(), Errno> {
        Err(Errno::EROFS)
    }

    /// `new` を `old` と同じファイルの、もう 1 つの名前にする(ハードリンク)
    fn link(&self, _old: &str, _new: &str) -> Result<(), Errno> {
        Err(Errno::EROFS)
    }

    /// `old` を `new` に移す(名前を変える)。`new` があれば置き換える。
    /// 同じパスや、自分の下への移動は呼ぶ側(`Vfs::rename`)が先に扱う。
    fn rename(&self, _old: &str, _new: &str) -> Result<(), Errno> {
        Err(Errno::EROFS)
    }
}

/// rename で `old` が `new` を置き換えてよいか(`new_empty`:`new` が空のディレクトリか)。
/// ファイルはファイルを、ディレクトリは空のディレクトリだけを置き換えられる。
pub fn check_replace(old: FileKind, new: FileKind, new_empty: bool) -> Result<(), Errno> {
    match (old == FileKind::Directory, new == FileKind::Directory) {
        (false, true) => Err(Errno::EISDIR),
        (true, false) => Err(Errno::ENOTDIR),
        (true, true) if !new_empty => Err(Errno::ENOTEMPTY),
        _ => Ok(()),
    }
}

/// マウントの表。長いマウントポイントから順に調べる。
pub struct Vfs {
    mounts: Vec<(String, Arc<dyn FileSystem>)>,
    /// dentry キャッシュ:リンクを含まない絶対パス → (実体, 種類)。見つかったものだけを覚える。
    /// 名前を変える操作(作る、消す、動かす、リンク)は全部 Vfs を通るので、そこで古い項目を消す
    dentries: crate::sync::SpinLock<Dentries>,
}

/// dentry キャッシュの中身:パス → (実体, 種類)。None は「その名前はない」(negative dentry)
type Dentries = alloc::collections::BTreeMap<String, Option<(Arc<dyn File>, FileKind)>>;

/// dentry キャッシュに覚える数の上限(超えたら全部忘れて覚え直す)
const DENTRY_LIMIT: usize = 1024;

impl Vfs {
    pub fn new(root: Arc<dyn FileSystem>) -> Self {
        Self {
            mounts: alloc::vec![(String::from("/"), root)],
            dentries: crate::sync::SpinLock::new(alloc::collections::BTreeMap::new()),
        }
    }

    pub fn mount(&mut self, at: &str, fs: Arc<dyn FileSystem>) {
        self.mounts.push((String::from(at), fs));
        self.mounts
            .sort_by_key(|(p, _)| core::cmp::Reverse(p.len()));
        self.dentries.lock().clear();
    }

    /// `path` のあるファイルシステムの番号(stat の st_dev)。マウントごとに違う
    /// `follow`:最後の要素がリンクなら追う
    pub fn device_of(&self, path: &str, follow: bool) -> Result<u64, Errno> {
        let path = self.resolve_path(path, follow)?;
        let (fs, _) = self.resolve(&path)?;
        let index = self
            .mounts
            .iter()
            .position(|(_, m)| core::ptr::addr_eq(Arc::as_ptr(m), fs as *const dyn FileSystem))
            .ok_or(Errno::ENOENT)?;
        // マウントの表は長い順に並べ替えるので、番号はマウント先の名前から作る(並べ替えで変わらない)
        let at = &self.mounts[index].0;
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in at.bytes() {
            h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
        }
        Ok(h & 0xffff_ffff)
    }

    /// `path` のあるファイルシステムの情報(リンクは追う)
    pub fn statfs(&self, path: &str) -> Result<StatFs, Errno> {
        let path = self.resolve_path(path, true)?;
        self.lookup(&path)?;
        let (fs, _) = self.resolve(&path)?;
        fs.statfs()
    }

    /// マウントしているすべてのファイルシステムを sync する
    pub fn sync(&self) -> Result<(), Errno> {
        for (_, fs) in &self.mounts {
            fs.sync()?;
        }
        Ok(())
    }

    /// `path` と、その下のすべての名前をキャッシュから消す
    fn forget(&self, path: &str) {
        let below = alloc::format!("{}/", path.trim_end_matches('/'));
        self.dentries
            .lock()
            .retain(|k, _| k != path && !k.starts_with(&below));
    }

    /// 絶対パスを、担当するファイルシステムとその中のパスに分ける。
    fn resolve<'a>(&'a self, path: &'a str) -> Result<(&'a dyn FileSystem, &'a str), Errno> {
        for (at, fs) in &self.mounts {
            let rest = if at == "/" {
                Some(path)
            } else {
                path.strip_prefix(at.as_str())
                    .filter(|r| r.is_empty() || r.starts_with('/'))
            };
            if let Some(rest) = rest {
                let inner = if rest.is_empty() { "/" } else { rest };
                return Ok((fs.as_ref(), inner));
            }
        }
        Err(Errno::ENOENT)
    }

    /// マウントの表だけで探す(シンボリックリンクは追わない)。`path` にリンクは含まれない前提
    fn lookup_raw(&self, path: &str) -> Result<Arc<dyn File>, Errno> {
        let (fs, inner) = self.resolve(path)?;
        fs.lookup(inner)
    }

    /// パスの途中のシンボリックリンクを追って、リンクを含まない絶対パスにする。
    /// `follow_last` なら最後の要素がリンクでも追う(open、stat)。追わないのは lstat、unlink、rename など。
    /// 最後の要素がなくてもよい(作るときのため)。追ったリンクが 40 を超えたら `ELOOP`。
    pub fn resolve_path(&self, path: &str, follow_last: bool) -> Result<String, Errno> {
        Ok(self.walk(path, follow_last)?.0)
    }

    /// `resolve_path` の中身。たどり着いた実体も返す(最後の要素がなければ None)。
    /// たどっている途中のディレクトリの実体を覚えておき、次の要素はその中から探す
    /// (ファイルシステムが `lookup_child` で、ルートからたどり直さずに済む)
    fn walk(
        &self,
        path: &str,
        follow_last: bool,
    ) -> Result<(String, Option<Arc<dyn File>>), Errno> {
        const MAX_LINKS: usize = 40;
        let mut todo: alloc::collections::VecDeque<String> = path
            .split('/')
            .filter(|c| !c.is_empty())
            .map(String::from)
            .collect();
        let mut cur = String::from("/");
        // cur の実体(None なら、必要になったときにマウントの表から探す)
        let mut cur_file: Option<Arc<dyn File>> = None;
        let mut links = 0;
        while let Some(comp) = todo.pop_front() {
            match comp.as_str() {
                "." => continue,
                ".." => {
                    cur = normalize(&cur, "..");
                    cur_file = None;
                    continue;
                }
                _ => {}
            }
            let next = normalize(&cur, &comp);
            let last = todo.is_empty();
            if last && !follow_last {
                let file = self.child(&cur, &mut cur_file, &comp, &next).ok();
                return Ok((next, file.map(|(f, _)| f)));
            }
            match self.child(&cur, &mut cur_file, &comp, &next) {
                Ok((f, FileKind::Symlink)) => {
                    links += 1;
                    if links > MAX_LINKS {
                        return Err(Errno::ELOOP);
                    }
                    let target = f.readlink()?;
                    // 絶対のリンクはルートから、相対のリンクはリンクのあるディレクトリから
                    if target.starts_with('/') {
                        cur = String::from("/");
                        cur_file = None;
                    }
                    for c in target.split('/').filter(|c| !c.is_empty()).rev() {
                        todo.push_front(String::from(c));
                    }
                }
                Ok((f, kind)) => {
                    if !last && kind != FileKind::Directory {
                        return Err(Errno::ENOTDIR);
                    }
                    cur = next;
                    cur_file = Some(f);
                }
                Err(Errno::ENOENT) if last => return Ok((next, None)),
                Err(e) => return Err(e),
            }
        }
        let file = match cur_file {
            Some(f) => f,
            None => self.lookup_raw(&cur)?,
        };
        Ok((cur, Some(file)))
    }

    /// ディレクトリ `cur`(実体は `cur_file`)の中の `name`(絶対パスでは `next`)を探し、
    /// 実体と種類を返す。dentry キャッシュにあればディスクを読まない
    fn child(
        &self,
        cur: &str,
        cur_file: &mut Option<Arc<dyn File>>,
        name: &str,
        next: &str,
    ) -> Result<(Arc<dyn File>, FileKind), Errno> {
        if let Some(hit) = self.dentries.lock().get(next) {
            return hit.clone().ok_or(Errno::ENOENT);
        }
        let (fs, inner) = self.resolve(next)?;
        let (parent_fs, parent_inner) = self.resolve(cur)?;
        let found = self.find(fs, inner, parent_fs, parent_inner, cur_file, name);
        let entry = match found {
            Ok(file) => {
                // 種類は変わらないので、実体と一緒に覚える(たどるたびに inode を読まずに済む)
                let kind = file.stat().kind;
                Some((file, kind))
            }
            Err(Errno::ENOENT) => None,
            Err(e) => return Err(e),
        };
        if fs.cacheable() {
            let mut d = self.dentries.lock();
            if d.len() >= DENTRY_LIMIT {
                d.clear();
            }
            d.insert(String::from(next), entry.clone());
        }
        entry.ok_or(Errno::ENOENT)
    }

    /// `child` の中身:ファイルシステムに聞く
    fn find(
        &self,
        fs: &dyn FileSystem,
        inner: &str,
        parent_fs: &dyn FileSystem,
        parent_inner: &str,
        cur_file: &mut Option<Arc<dyn File>>,
        name: &str,
    ) -> Result<Arc<dyn File>, Errno> {
        // マウントポイントをまたぐときは、マウントの表から探す
        if inner == "/" || !core::ptr::addr_eq(fs, parent_fs) {
            fs.lookup(inner)
        } else {
            let dir = match cur_file {
                Some(d) => d.clone(),
                None => {
                    let d = parent_fs.lookup(parent_inner)?;
                    *cur_file = Some(d.clone());
                    d
                }
            };
            fs.lookup_child(&dir, parent_inner, name)
        }
    }

    /// 絶対パスの実体を探す(リンクは最後まで追う)。
    pub fn lookup(&self, path: &str) -> Result<Arc<dyn File>, Errno> {
        self.walk(path, true)?.1.ok_or(Errno::ENOENT)
    }

    /// 最後の要素がリンクなら、リンクそのものを返す(lstat、readlink)
    pub fn lookup_nofollow(&self, path: &str) -> Result<Arc<dyn File>, Errno> {
        self.walk(path, false)?.1.ok_or(Errno::ENOENT)
    }

    /// ファイルを作る。最後がリンク(指す先がない)なら、指す先に作る
    pub fn create(&self, path: &str, mode: u32) -> Result<Arc<dyn File>, Errno> {
        let path = self.resolve_path(path, true)?;
        let (fs, inner) = self.resolve(&path)?;
        self.forget(&path);
        fs.create(inner, mode)
    }

    pub fn mkdir(&self, path: &str, mode: u32) -> Result<(), Errno> {
        let path = self.resolve_path(path, false)?;
        let (fs, inner) = self.resolve(&path)?;
        self.forget(&path);
        fs.mkdir(inner, mode)
    }

    /// 消す(最後がリンクなら、リンクそのものを消す)
    pub fn unlink(&self, path: &str, dir: bool) -> Result<(), Errno> {
        let path = self.resolve_path(path, false)?;
        let (fs, inner) = self.resolve(&path)?;
        let r = fs.unlink(inner, dir);
        self.forget(&path);
        r
    }

    /// 名前付きパイプなどを作る
    pub fn mknod(&self, path: &str, kind: FileKind, mode: u32) -> Result<(), Errno> {
        let path = self.resolve_path(path, false)?;
        let (fs, inner) = self.resolve(&path)?;
        if fs.lookup(inner).is_ok() {
            return Err(Errno::EEXIST);
        }
        self.forget(&path);
        fs.mknod(inner, kind, mode)
    }

    /// `path` に `target` を指すシンボリックリンクを作る
    pub fn symlink(&self, target: &str, path: &str) -> Result<(), Errno> {
        let path = self.resolve_path(path, false)?;
        let (fs, inner) = self.resolve(&path)?;
        if fs.lookup(inner).is_ok() {
            return Err(Errno::EEXIST);
        }
        self.forget(&path);
        fs.symlink(inner, target)
    }

    /// `new` を `old` のハードリンクにする。`follow` なら `old` が最後にリンクでも追う
    pub fn link(&self, old: &str, new: &str, follow: bool) -> Result<(), Errno> {
        let old = &self.resolve_path(old, follow)?;
        let new = &self.resolve_path(new, false)?;
        let (old_fs, old_inner) = self.resolve(old)?;
        let (new_fs, new_inner) = self.resolve(new)?;
        let file = old_fs.lookup(old_inner)?;
        if !core::ptr::addr_eq(old_fs, new_fs) {
            return Err(Errno::EXDEV);
        }
        if file.stat().kind == FileKind::Directory {
            return Err(Errno::EPERM);
        }
        if new_fs.lookup(new_inner).is_ok() {
            return Err(Errno::EEXIST);
        }
        self.forget(new);
        old_fs.link(old_inner, new_inner)
    }

    /// `old` を `new` に移す。`noreplace` なら、`new` があるときは `EEXIST`。
    /// どちらも最後の要素は追わない(リンクを動かすと、リンクそのものが動く)。
    pub fn rename(&self, old: &str, new: &str, noreplace: bool) -> Result<(), Errno> {
        let old = &self.resolve_path(old, false)?;
        let new = &self.resolve_path(new, false)?;
        let (old_fs, old_inner) = self.resolve(old)?;
        let (new_fs, new_inner) = self.resolve(new)?;
        old_fs.lookup(old_inner)?;
        // マウントポイントを動かすことも、ファイルシステムをまたぐこともできない
        if !core::ptr::addr_eq(old_fs, new_fs) || old_inner == "/" || new_inner == "/" {
            return Err(Errno::EXDEV);
        }
        if old == new {
            return Ok(());
        }
        // 自分の下には移せない(/a → /a/b)
        if new.starts_with(old.as_str()) && new.as_bytes().get(old.len()) == Some(&b'/') {
            return Err(Errno::EINVAL);
        }
        if noreplace && new_fs.lookup(new_inner).is_ok() {
            return Err(Errno::EEXIST);
        }
        let r = old_fs.rename(old_inner, new_inner);
        self.forget(old);
        self.forget(new);
        r
    }

    /// ディレクトリの中身。マウントポイントもその名前の項目として見せる。
    pub fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, Errno> {
        let path = &self.resolve_path(path, true)?;
        let (fs, inner) = self.resolve(path)?;
        let mut entries = fs.readdir(inner)?;
        for (at, _) in &self.mounts {
            if let Some(name) = at.strip_prefix(path).map(|r| r.trim_start_matches('/'))
                && !name.is_empty()
                && !name.contains('/')
                && at != path
                && !entries.iter().any(|e| e.name == name)
            {
                entries.push(DirEntry {
                    name: String::from(name),
                    kind: FileKind::Directory,
                    ino: 0,
                });
            }
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
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
                uid: 0,
                gid: 0,
                ..Default::default()
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

    /// 中身が外から変わるファイルシステム(/proc のプロセスのように)の代わり
    struct Changing(crate::sync::SpinLock<bool>);

    impl FileSystem for Changing {
        fn lookup(&self, path: &str) -> Result<Arc<dyn File>, Errno> {
            match (path, *self.0.lock()) {
                ("/", _) => crate::fs::tmpfs::Tmpfs::new().lookup("/"),
                ("/now", true) => crate::fs::tmpfs::Tmpfs::new().lookup("/"),
                _ => Err(Errno::ENOENT),
            }
        }
    }

    fn tmp_vfs() -> Vfs {
        let tmp = crate::fs::tmpfs::Tmpfs::new();
        tmp.mkdir("/d", 0o755).unwrap();
        tmp.create("/d/x", 0o644).unwrap();
        tmp.create("/a", 0o644).unwrap();
        Vfs::new(Arc::new(tmp))
    }

    #[test]
    fn cached_names_are_forgotten_when_they_change() {
        let vfs = tmp_vfs();
        assert!(vfs.lookup("/a").is_ok());
        assert!(vfs.lookup("/d/x").is_ok());
        vfs.unlink("/a", false).unwrap();
        assert_eq!(vfs.lookup("/a").err(), Some(Errno::ENOENT));
        // ディレクトリを動かすと、中の名前も古いパスでは見つからない
        vfs.rename("/d", "/e", false).unwrap();
        assert_eq!(vfs.lookup("/d/x").err(), Some(Errno::ENOENT));
        assert!(vfs.lookup("/e/x").is_ok());
        // 同じ名前で作り直したら、新しいほうが見える
        vfs.create("/a", 0o600).unwrap();
        assert_eq!(vfs.lookup("/a").unwrap().stat().mode, 0o600);
        vfs.symlink("/e", "/l").unwrap();
        assert!(vfs.lookup("/l/x").is_ok());
        vfs.unlink("/l", false).unwrap();
        assert_eq!(vfs.lookup("/l/x").err(), Some(Errno::ENOENT));
    }

    #[test]
    fn remembered_missing_names_are_forgotten_when_created() {
        let vfs = tmp_vfs();
        for (path, make) in [("/n1", 0), ("/n2", 1), ("/n3", 2), ("/n4", 3), ("/n5", 4)] {
            assert_eq!(vfs.lookup(path).err(), Some(Errno::ENOENT));
            match make {
                0 => drop(vfs.create(path, 0o644).unwrap()),
                1 => vfs.mkdir(path, 0o755).unwrap(),
                2 => vfs.symlink("/a", path).unwrap(),
                3 => vfs.link("/a", path, false).unwrap(),
                _ => vfs.rename("/a", path, false).unwrap(),
            }
            assert!(vfs.lookup(path).is_ok(), "{path}");
        }
        // 「ない」ディレクトリの下も「ない」
        assert_eq!(vfs.lookup("/nodir/x").err(), Some(Errno::ENOENT));
        vfs.mkdir("/nodir", 0o755).unwrap();
        vfs.create("/nodir/x", 0o644).unwrap();
        assert!(vfs.lookup("/nodir/x").is_ok());
    }

    #[test]
    fn filesystems_that_change_by_themselves_are_not_cached() {
        let fs = Arc::new(Changing(crate::sync::SpinLock::new(true)));
        let mut vfs = tmp_vfs();
        vfs.mount("/proc", fs.clone());
        assert!(vfs.lookup("/proc/now").is_ok());
        *fs.0.lock() = false; // プロセスが終わった
        assert_eq!(vfs.lookup("/proc/now").err(), Some(Errno::ENOENT));
    }
}
