//! プログラムの実行(ELF の読み込みと初期スタック)。

pub mod elf;

use crate::Errno;
use crate::mm::user::UserMemory;
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
pub fn load(
    mem: &mut UserMemory,
    image: &[u8],
    argv: &[&[u8]],
    envp: &[&[u8]],
    random: [u8; 16],
) -> Result<Loaded, Errno> {
    let elf = elf::parse(image)?;
    let mut brk = 0;
    for seg in elf.segments() {
        // 1つのページを2つのセグメントが共有することがある。書き込める方の権限でまとめる
        let flags = if seg.flags.write {
            MapFlags::USER_RW
        } else {
            seg.flags
        };
        mem.map_zeroed(VirtAddr(seg.vaddr), seg.memsz, flags)?;
        mem.load(VirtAddr(seg.vaddr), seg.data)?;
        brk = brk.max(VirtAddr(seg.vaddr + seg.memsz).align_up().0);
    }
    mem.map_zeroed(
        VirtAddr(STACK_TOP - STACK_SIZE),
        STACK_SIZE,
        MapFlags::USER_RW,
    )?;
    let sp = build_stack(mem, &elf, argv, envp, random)?;
    Ok(Loaded {
        entry: elf.entry,
        sp,
        brk,
    })
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
    elf: &elf::Elf<'_>,
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
    if let Some(phdr) = elf.phdr_vaddr {
        aux.push((AT_PHDR, phdr));
    }
    aux.extend([
        (AT_PHENT, elf.phent as u64),
        (AT_PHNUM, elf.phnum as u64),
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
mod test_elf;

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
        let loaded = load(&mut mem, &sample(), &[b"prog"], &[], [7; 16]).unwrap();
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
        load(&mut mem, &sample(), &[b"prog"], &[], [0; 16]).unwrap();
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
            &sample(),
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
            load(&mut mem, b"garbage", &[], &[], [0; 16]).err(),
            Some(Errno::ENOEXEC)
        );
        assert_eq!(phys.live_frames(), 0);
    }
}
