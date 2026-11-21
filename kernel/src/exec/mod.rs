//! プログラムの実行(ELF の読み込みと初期スタック)。

pub mod elf;

use crate::Errno;
use crate::mm::user::UserMemory;
use alloc::string::String;
use alloc::vec::Vec;
use hal::addr::{PAGE_SIZE, VirtAddr};
use hal::mmu::MapFlags;

/// ユーザースタックの一番上と大きさ
pub const STACK_TOP: u64 = 0x0000_7fff_ffff_0000;
pub const STACK_SIZE: u64 = 1024 * 1024;

// 補助ベクタ(auxv)の種類。カーネルがプログラムに渡す追加情報
pub const AT_NULL: u64 = 0;
pub const AT_PHDR: u64 = 3;
pub const AT_PHENT: u64 = 4;
pub const AT_PHNUM: u64 = 5;
pub const AT_PAGESZ: u64 = 6;
pub const AT_BASE: u64 = 7;
pub const AT_ENTRY: u64 = 9;
pub const AT_UID: u64 = 11;
pub const AT_EUID: u64 = 12;
pub const AT_GID: u64 = 13;
pub const AT_EGID: u64 = 14;
pub const AT_SECURE: u64 = 23;
pub const AT_RANDOM: u64 = 25;

/// 読み込みの結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loaded {
    /// 実行を始めるアドレス
    pub entry: u64,
    /// 初期のスタックポインタ(argc を指す)
    pub sp: u64,
    /// ヒープ(brk)の始まり
    pub brk: u64,
}

/// ELF の `image` を `mem` に読み込み、`argv` と `envp` を積んだスタックを作る。
/// `random` は AT_RANDOM で渡す 16 バイト(musl がスタック保護の値などに使う)。
/// 実行ファイルの中身。initramfs ならメモリの中をそのまま指し、それ以外(tmpfs、ext2)は読んで持つ。
pub fn read_image(
    file: &dyn crate::fs::vfs::File,
) -> Result<alloc::borrow::Cow<'static, [u8]>, crate::Errno> {
    if let Some(bytes) = file.contents() {
        // SAFETY: contents を見せるのは initramfs だけで、その中身は 'static な埋め込みデータ
        let bytes: &'static [u8] =
            unsafe { core::slice::from_raw_parts(bytes.as_ptr(), bytes.len()) };
        return Ok(alloc::borrow::Cow::Borrowed(bytes));
    }
    let size = file.stat().size as usize;
    let mut buf = alloc::vec![0u8; size];
    let mut done = 0;
    while done < size {
        match file.read(done as u64, &mut buf[done..])? {
            0 => break,
            n => done += n,
        }
    }
    buf.truncate(done);
    Ok(alloc::borrow::Cow::Owned(buf))
}

/// 実行ファイル:好きな位置から読めるもの。中身を全部メモリに置かなくても読み込める
/// (大きなプログラムを、カーネルのヒープに丸ごと読まずに済む)。
pub trait Image {
    fn size(&self) -> u64;
    /// `offset` から `buf` に読み、読めたバイト数を返す(終わりなら 0)
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno>;
}

impl Image for [u8] {
    fn size(&self) -> u64 {
        self.len() as u64
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        let start = (offset as usize).min(self.len());
        let n = buf.len().min(self.len() - start);
        buf[..n].copy_from_slice(&self[start..start + n]);
        Ok(n)
    }
}

impl Image for dyn crate::fs::vfs::File {
    fn size(&self) -> u64 {
        self.stat().size
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        self.read(offset, buf)
    }
}

/// `offset` から `buf` を埋めるまで読む(足りなければ ENOEXEC)
fn read_exact(image: &(impl Image + ?Sized), offset: u64, buf: &mut [u8]) -> Result<(), Errno> {
    let mut done = 0;
    while done < buf.len() {
        match image.read_at(offset + done as u64, &mut buf[done..])? {
            0 => return Err(Errno::ENOEXEC),
            n => done += n,
        }
    }
    Ok(())
}

/// 一度に読む大きさ(1 ページ)
const CHUNK: usize = 4096;

/// 動的リンカを置く場所(mmap の領域の手前)
pub const INTERP_BASE: u64 = 0x0000_3f00_0000_0000;
/// 位置独立(ET_DYN)のプログラムを置く場所
pub const PIE_BASE: u64 = 0x0000_2aaa_0000_0000;

/// ELF ヘッダとプログラムヘッダだけを読む
fn read_headers(image: &(impl Image + ?Sized)) -> Result<Vec<u8>, Errno> {
    let size = image.size();
    let mut head = alloc::vec![0u8; (size as usize).min(64)];
    read_exact(image, 0, &mut head)?;
    let len = elf::headers_len(&head).ok_or(Errno::ENOEXEC)?;
    if len as u64 > size || len > 64 * 1024 {
        return Err(Errno::ENOEXEC);
    }
    if len > head.len() {
        head.resize(len, 0);
        read_exact(image, 64, &mut head[64..])?;
    }
    Ok(head)
}

/// 動的リンクのプログラムなら、インタプリタ(動的リンカ)のパス(PT_INTERP)
pub fn interpreter(image: &(impl Image + ?Sized)) -> Result<Option<String>, Errno> {
    let head = read_headers(image)?;
    let elf = elf::parse(&head, image.size())?;
    let Some((offset, len)) = elf.interp else {
        return Ok(None);
    };
    let mut path = alloc::vec![0u8; len as usize];
    read_exact(image, offset, &mut path)?;
    // 終わりの NUL を外す
    let path = path.split(|&b| b == 0).next().unwrap_or(&[]);
    let path = core::str::from_utf8(path).map_err(|_| Errno::ENOEXEC)?;
    Ok(Some(String::from(path)))
}

/// セグメントを `bias` だけずらして読み込み、データの終わり(ページ境界)を返す
fn load_segments(
    mem: &mut UserMemory,
    image: &(impl Image + ?Sized),
    elf: &elf::Elf<'_>,
    bias: u64,
) -> Result<u64, Errno> {
    let mut end = 0;
    for seg in elf.segments() {
        let vaddr = seg.vaddr.checked_add(bias).ok_or(Errno::ENOEXEC)?;
        if vaddr
            .checked_add(seg.memsz)
            .is_none_or(|e| e > elf::USER_END)
        {
            return Err(Errno::ENOEXEC);
        }
        // 1つのページを2つのセグメントが共有することがある。書き込める方の権限でまとめる
        let flags = if seg.flags.write {
            MapFlags::USER_RW
        } else {
            seg.flags
        };
        mem.map_zeroed(VirtAddr(vaddr), seg.memsz, flags)?;
        // 中身はファイルから 1 ページずつ写す
        let mut chunk = [0u8; CHUNK];
        let mut done = 0;
        while done < seg.filesz {
            let n = (seg.filesz - done).min(CHUNK as u64) as usize;
            read_exact(image, seg.offset + done, &mut chunk[..n])?;
            mem.load(VirtAddr(vaddr + done), &chunk[..n])?;
            done += n as u64;
        }
        end = end.max(VirtAddr(vaddr + seg.memsz).align_up().0);
    }
    Ok(end)
}

/// 静的リンクのプログラムを読み込む(動的リンクなら `ENOEXEC`:`load_with_interp` を使う)。
pub fn load(
    mem: &mut UserMemory,
    image: &(impl Image + ?Sized),
    argv: &[&[u8]],
    envp: &[&[u8]],
    random: [u8; 16],
) -> Result<Loaded, Errno> {
    load_with_interp(mem, image, None::<&[u8]>, argv, envp, random)
}

/// プログラムを読み込む。動的リンクなら `interp`(PT_INTERP のパスのファイル)も読み込み、
/// 動的リンカの入口から始める。動的リンカは補助ベクタの AT_BASE(自分の場所)、AT_PHDR、
/// AT_ENTRY(本体の入口)を見て、共有ライブラリを読み込んでから本体に飛ぶ。
pub fn load_with_interp<I: Image + ?Sized>(
    mem: &mut UserMemory,
    image: &(impl Image + ?Sized),
    interp: Option<&I>,
    argv: &[&[u8]],
    envp: &[&[u8]],
    random: [u8; 16],
) -> Result<Loaded, Errno> {
    let head = read_headers(image)?;
    let elf = elf::parse(&head, image.size())?;
    let bias = if elf.position_independent {
        PIE_BASE
    } else {
        0
    };
    let brk = load_segments(mem, image, &elf, bias)?;
    let main_entry = elf.entry + bias;
    let (start, base) = match (elf.interp, interp) {
        (None, _) => (main_entry, None),
        (Some(_), None) => return Err(Errno::ENOEXEC),
        (Some(_), Some(ld)) => {
            let ld_head = read_headers(ld)?;
            let ld_elf = elf::parse(&ld_head, ld.size())?;
            // 動的リンカ自身は、ほかのインタプリタを要らない位置独立のもの
            if ld_elf.interp.is_some() || !ld_elf.position_independent {
                return Err(Errno::ENOEXEC);
            }
            load_segments(mem, ld, &ld_elf, INTERP_BASE)?;
            (ld_elf.entry + INTERP_BASE, Some(INTERP_BASE))
        }
    };
    mem.map_zeroed(
        VirtAddr(STACK_TOP - STACK_SIZE),
        STACK_SIZE,
        MapFlags::USER_RW,
    )?;
    let at = AuxInfo {
        phdr: elf.phdr_vaddr.map(|p| p + bias),
        phent: elf.phent as u64,
        phnum: elf.phnum as u64,
        entry: main_entry,
        base,
    };
    let sp = build_stack(mem, &at, argv, envp, random)?;
    Ok(Loaded {
        entry: start,
        sp,
        brk,
    })
}

/// 補助ベクタに書く、プログラムの情報
struct AuxInfo {
    phdr: Option<u64>,
    phent: u64,
    phnum: u64,
    entry: u64,
    /// 動的リンカを置いた場所
    base: Option<u64>,
}

/// 初期スタックを組み立てる。上(高いアドレス)から順に:
///
/// ```text
/// 文字列(argv、envp)と AT_RANDOM の 16 バイト
/// (16 バイト境界までの詰め物)
/// 補助ベクタ … AT_NULL
/// envp[0] … NULL
/// argv[0] … NULL
/// argc                     ← sp
/// ```
fn build_stack(
    mem: &mut UserMemory,
    elf: &AuxInfo,
    argv: &[&[u8]],
    envp: &[&[u8]],
    random: [u8; 16],
) -> Result<u64, Errno> {
    let mut top = STACK_TOP;
    let mut push_bytes = |mem: &mut UserMemory, bytes: &[u8]| -> Result<u64, Errno> {
        top -= bytes.len() as u64;
        mem.copy_to_user(VirtAddr(top), bytes)?;
        Ok(top)
    };
    let random_ptr = push_bytes(mem, &random)?;
    let mut push_strings = |mem: &mut UserMemory, list: &[&[u8]]| -> Result<Vec<u64>, Errno> {
        list.iter()
            .map(|s| {
                push_bytes(mem, &[0])?;
                push_bytes(mem, s)
            })
            .collect()
    };
    let argv_ptrs = push_strings(mem, argv)?;
    let envp_ptrs = push_strings(mem, envp)?;

    let mut words: Vec<u64> = Vec::new();
    words.push(argv.len() as u64);
    words.extend(&argv_ptrs);
    words.push(0);
    words.extend(&envp_ptrs);
    words.push(0);
    let mut aux = Vec::new();
    if let Some(phdr) = elf.phdr {
        aux.push((AT_PHDR, phdr));
    }
    if let Some(base) = elf.base {
        aux.push((AT_BASE, base));
    }
    aux.extend([
        (AT_PHENT, elf.phent),
        (AT_PHNUM, elf.phnum),
        (AT_PAGESZ, PAGE_SIZE),
        (AT_ENTRY, elf.entry),
        (AT_UID, 0),
        (AT_EUID, 0),
        (AT_GID, 0),
        (AT_EGID, 0),
        (AT_SECURE, 0),
        (AT_RANDOM, random_ptr),
        (AT_NULL, 0),
    ]);
    for (k, v) in aux {
        words.extend([k, v]);
    }

    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    // sp は 16 バイト境界(AArch64 の呼び出し規約)
    let sp = (top - bytes.len() as u64) & !15;
    mem.copy_to_user(VirtAddr(sp), &bytes)?;
    Ok(sp)
}

#[cfg(test)]
pub(crate) mod test_elf;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Errno;
    use crate::exec::elf::{EM_AARCH64, PF_R, PF_W, PF_X, PT_LOAD};
    use crate::exec::test_elf::build;
    use crate::mm::user::tests::memory;
    use hal::addr::VirtAddr;

    fn u64_at(mem: &mut crate::mm::user::UserMemory, va: u64) -> u64 {
        let mut b = [0u8; 8];
        mem.copy_from_user(VirtAddr(va), &mut b).unwrap();
        u64::from_le_bytes(b)
    }

    fn sample() -> std::vec::Vec<u8> {
        build(
            EM_AARCH64,
            &[
                (PT_LOAD, PF_R | PF_X, 0x40_0000, b"CODE", 4),
                (PT_LOAD, PF_R | PF_W, 0x41_0010, b"DATA", 0x3000),
            ],
        )
    }

    #[test]
    fn loads_segments_and_zero_fills_bss() {
        let (mut mem, _) = memory();
        let loaded = load(&mut mem, sample().as_slice(), &[b"prog"], &[], [7; 16]).unwrap();
        assert_eq!(loaded.entry, 0x40_1000);
        let mut buf = [0u8; 4];
        mem.copy_from_user(VirtAddr(0x40_0000), &mut buf).unwrap();
        assert_eq!(&buf, b"CODE");
        mem.copy_from_user(VirtAddr(0x41_0010), &mut buf).unwrap();
        assert_eq!(&buf, b"DATA");
        mem.copy_from_user(VirtAddr(0x41_2000), &mut buf).unwrap();
        assert_eq!(buf, [0; 4]);
        // brk はデータの終わり(ページ境界に切り上げ)
        assert_eq!(loaded.brk, 0x41_4000);
    }

    #[test]
    fn code_pages_are_not_writable_by_the_user() {
        let (mut mem, _) = memory();
        load(&mut mem, sample().as_slice(), &[b"prog"], &[], [0; 16]).unwrap();
        assert_eq!(
            mem.copy_to_user(VirtAddr(0x40_0000), b"x"),
            Err(Errno::EFAULT)
        );
    }

    #[test]
    fn initial_stack_has_argc_argv_envp_and_auxv() {
        let (mut mem, _) = memory();
        let loaded = load(
            &mut mem,
            sample().as_slice(),
            &[b"qjs", b"hello.js"],
            &[b"HOME=/"],
            [9; 16],
        )
        .unwrap();
        let sp = loaded.sp;
        assert_eq!(sp % 16, 0);
        assert!(sp < STACK_TOP && sp > STACK_TOP - STACK_SIZE);
        assert_eq!(u64_at(&mut mem, sp), 2); // argc
        let argv0 = u64_at(&mut mem, sp + 8);
        let argv1 = u64_at(&mut mem, sp + 16);
        assert_eq!(u64_at(&mut mem, sp + 24), 0);
        assert_eq!(mem.read_cstr(VirtAddr(argv0), 64).unwrap(), b"qjs");
        assert_eq!(mem.read_cstr(VirtAddr(argv1), 64).unwrap(), b"hello.js");
        let env0 = u64_at(&mut mem, sp + 32);
        assert_eq!(u64_at(&mut mem, sp + 40), 0);
        assert_eq!(mem.read_cstr(VirtAddr(env0), 64).unwrap(), b"HOME=/");

        // 補助ベクタ(種類, 値)の組を AT_NULL まで読む
        let mut aux = std::collections::BTreeMap::new();
        let mut p = sp + 48;
        loop {
            let (k, v) = (u64_at(&mut mem, p), u64_at(&mut mem, p + 8));
            if k == AT_NULL {
                break;
            }
            aux.insert(k, v);
            p += 16;
        }
        assert_eq!(aux[&AT_PAGESZ], 4096);
        assert_eq!(aux[&AT_ENTRY], 0x40_1000);
        assert_eq!(aux[&AT_PHENT], 56);
        assert_eq!(aux[&AT_PHNUM], 2);
        let mut random = [0u8; 16];
        mem.copy_from_user(VirtAddr(aux[&AT_RANDOM]), &mut random)
            .unwrap();
        assert_eq!(random, [9; 16]);
    }

    #[test]
    fn invalid_images_are_rejected_without_mapping_anything() {
        let (mut mem, phys) = memory();
        assert_eq!(
            load(&mut mem, &b"garbage"[..], &[], &[], [0; 16]).err(),
            Some(Errno::ENOEXEC)
        );
        assert_eq!(phys.live_frames(), 0);
    }

    /// 読まれた長さの最大を覚える実行ファイル
    struct Counting {
        bytes: std::vec::Vec<u8>,
        largest: core::cell::Cell<usize>,
    }

    impl Image for Counting {
        fn size(&self) -> u64 {
            self.bytes.len() as u64
        }
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
            self.largest.set(self.largest.get().max(buf.len()));
            self.bytes.as_slice().read_at(offset, buf)
        }
    }

    #[test]
    fn big_segments_are_copied_in_pieces_not_read_whole() {
        // 64KiB のデータを持つプログラムでも、カーネルは 1 ページずつしか読まない
        let data: std::vec::Vec<u8> = (0..65536u32).map(|i| (i % 251) as u8).collect();
        let image = Counting {
            bytes: build(
                EM_AARCH64,
                &[(PT_LOAD, PF_R | PF_W, 0x40_0000, &data, 65536)],
            ),
            largest: core::cell::Cell::new(0),
        };
        let (mut mem, _) = memory();
        load(&mut mem, &image, &[b"big"], &[], [0; 16]).unwrap();
        assert!(
            image.largest.get() <= 4096,
            "read {} bytes at once",
            image.largest.get()
        );
        let mut buf = [0u8; 16];
        mem.copy_from_user(VirtAddr(0x40_0000 + 60000), &mut buf)
            .unwrap();
        assert_eq!(&buf[..], &data[60000..60016]);
    }

    const INTERP: &[u8] = b"/lib/ld-musl-aarch64.so.1\0";

    /// 動的リンクのプログラム:PT_INTERP と、0x40_0000 のコード
    fn dynamic_program() -> std::vec::Vec<u8> {
        use crate::exec::elf::PT_INTERP;
        build(
            EM_AARCH64,
            &[
                (PT_INTERP, PF_R, 0, INTERP, INTERP.len() as u64),
                (PT_LOAD, PF_R | PF_X, 0x40_0000, b"MAIN", 4),
            ],
        )
    }

    /// 位置独立(ET_DYN)の動的リンカ:アドレス 0 からのコード、入口は 0x100
    fn interpreter_image() -> std::vec::Vec<u8> {
        crate::exec::test_elf::build_typed(
            EM_AARCH64,
            3,
            0x100,
            &[(PT_LOAD, PF_R | PF_X, 0, b"LDSO", 0x1000)],
        )
    }

    fn aux_of(
        mem: &mut crate::mm::user::UserMemory,
        sp: u64,
    ) -> std::collections::BTreeMap<u64, u64> {
        // argc、argv、NULL、envp、NULL の後ろが補助ベクタ
        let argc = u64_at(mem, sp);
        let mut p = sp + 8 * (argc + 2);
        while u64_at(mem, p) != 0 {
            p += 8;
        }
        p += 8;
        let mut aux = std::collections::BTreeMap::new();
        while u64_at(mem, p) != AT_NULL {
            aux.insert(u64_at(mem, p), u64_at(mem, p + 8));
            p += 16;
        }
        aux
    }

    #[test]
    fn the_interpreter_path_is_read_from_pt_interp() {
        assert_eq!(
            interpreter(dynamic_program().as_slice())
                .unwrap()
                .as_deref(),
            Some("/lib/ld-musl-aarch64.so.1")
        );
        assert_eq!(interpreter(sample().as_slice()).unwrap(), None);
    }

    #[test]
    fn a_dynamic_program_starts_in_its_interpreter() {
        let (mut mem, _) = memory();
        let ld = interpreter_image();
        let loaded = load_with_interp(
            &mut mem,
            dynamic_program().as_slice(),
            Some(ld.as_slice()),
            &[b"prog"],
            &[],
            [0; 16],
        )
        .unwrap();
        // 動き出すのは動的リンカの入口(置いた場所 + 0x100)
        assert_eq!(loaded.entry, INTERP_BASE + 0x100);
        let mut buf = [0u8; 4];
        mem.copy_from_user(VirtAddr(INTERP_BASE), &mut buf).unwrap();
        assert_eq!(&buf, b"LDSO");
        mem.copy_from_user(VirtAddr(0x40_0000), &mut buf).unwrap();
        assert_eq!(&buf, b"MAIN");
        // 動的リンカは補助ベクタで、自分の場所(AT_BASE)と本体の入口(AT_ENTRY)を知る
        let aux = aux_of(&mut mem, loaded.sp);
        assert_eq!(aux[&AT_BASE], INTERP_BASE);
        assert_eq!(aux[&AT_ENTRY], 0x40_1000);
    }

    #[test]
    fn a_dynamic_program_without_its_interpreter_is_enoexec() {
        let (mut mem, _) = memory();
        assert_eq!(
            load(
                &mut mem,
                dynamic_program().as_slice(),
                &[b"prog"],
                &[],
                [0; 16]
            )
            .err(),
            Some(Errno::ENOEXEC)
        );
    }

    #[test]
    fn a_position_independent_program_is_moved_to_its_base() {
        // ld-musl を直接実行するときなど:ET_DYN のプログラムは PIE_BASE にずらして置く
        let (mut mem, _) = memory();
        let loaded = load(
            &mut mem,
            interpreter_image().as_slice(),
            &[b"ld.so"],
            &[],
            [0; 16],
        )
        .unwrap();
        assert_eq!(loaded.entry, PIE_BASE + 0x100);
        let mut buf = [0u8; 4];
        mem.copy_from_user(VirtAddr(PIE_BASE), &mut buf).unwrap();
        assert_eq!(&buf, b"LDSO");
        assert_eq!(aux_of(&mut mem, loaded.sp)[&AT_ENTRY], PIE_BASE + 0x100);
    }
}
