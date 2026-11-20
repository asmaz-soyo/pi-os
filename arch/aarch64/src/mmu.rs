//! AArch64 のページテーブル(4KiB グラニュール、48 ビットの仮想アドレス、4 段)。
//!
//! 仮想アドレスは 9 ビットずつ 4 段のテーブルの添字になり、最後の 12 ビットがページ内の位置になる。
//!
//! ```text
//!  47      39 38      30 29      21 20      12 11         0
//! | L0 添字 | L1 添字 | L2 添字 | L3 添字 | ページ内オフセット |
//! ```
//!
//! 各テーブルは 512 個の 8 バイトの記述子(4KiB)。記述子は「次のテーブル」か「ここで終わり」
//! (L1 なら 1GiB ブロック、L2 なら 2MiB ブロック、L3 なら 4KiB ページ)を指す。
//!
//! 上位 16 ビットが全部 1 のアドレス(`0xFFFF_...`)は TTBR1(カーネル)、
//! 全部 0 のアドレスは TTBR0(ユーザー)のテーブルで変換される。

use hal::addr::{PAGE_SIZE, PhysAddr, VirtAddr};
use hal::mmu::MapFlags;

/// カーネルの仮想アドレスの始まり。物理アドレス `pa` は `KERNEL_VBASE + pa` に見える(線形マップ)。
pub const KERNEL_VBASE: u64 = 0xFFFF_0000_0000_0000;

/// テーブル1枚の記述子の数
pub const ENTRIES: usize = 512;

const VALID: u64 = 1 << 0;
const TABLE_OR_PAGE: u64 = 1 << 1;
const ATTR_DEVICE: u64 = 0 << 2; // MAIR の Attr0
const ATTR_NORMAL: u64 = 1 << 2; // MAIR の Attr1
const AP_EL0: u64 = 1 << 6; // EL0 からも触れる
const AP_RO: u64 = 1 << 7; // 読み出し専用
const SH_INNER: u64 = 0b11 << 8;
const AF: u64 = 1 << 10; // アクセスフラグ(0 だと最初のアクセスでフォールトする)
/// not Global:ASID ごとのマッピング(ユーザー空間)
pub const NG: u64 = 1 << 11;
/// EL1 では実行しない
pub const PXN: u64 = 1 << 53;
/// EL0 では実行しない
pub const UXN: u64 = 1 << 54;
const ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;

/// MAIR_EL1 に書く値。Attr0 = Device-nGnRE、Attr1 = Normal(Write-Back キャッシュ)。
pub const MAIR_VALUE: u64 = 0xff_04;

/// テーブルの段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    L0,
    L1,
    L2,
    L3,
}

impl Level {
    fn shift(self) -> u32 {
        match self {
            Level::L0 => 39,
            Level::L1 => 30,
            Level::L2 => 21,
            Level::L3 => 12,
        }
    }

    fn index(self, va: VirtAddr) -> usize {
        ((va.0 >> self.shift()) & 0x1ff) as usize
    }

    fn block_size(self) -> u64 {
        1 << self.shift()
    }
}

pub const fn phys_to_virt(pa: PhysAddr) -> VirtAddr {
    VirtAddr(pa.0 + KERNEL_VBASE)
}

pub const fn virt_to_phys(va: VirtAddr) -> PhysAddr {
    PhysAddr(va.0 - KERNEL_VBASE)
}

/// 「ここで終わり」の記述子(L3 ならページ、L1/L2 ならブロック)。
pub fn leaf_descriptor(pa: PhysAddr, flags: MapFlags, level: Level) -> u64 {
    let mut d = (pa.0 & ADDR_MASK) | VALID | AF;
    if level == Level::L3 {
        d |= TABLE_OR_PAGE;
    }
    if flags.device {
        d |= ATTR_DEVICE;
    } else {
        d |= ATTR_NORMAL | SH_INNER;
    }
    if !flags.write {
        d |= AP_RO;
    }
    if flags.user {
        d |= AP_EL0 | NG;
    }
    // ユーザーのページはカーネルからは実行させない(PXN)。逆も同じ(UXN)
    let el1_exec = flags.exec && !flags.user && !flags.device;
    let el0_exec = flags.exec && flags.user && !flags.device;
    if !el1_exec {
        d |= PXN;
    }
    if !el0_exec {
        d |= UXN;
    }
    d
}

/// 記述子の属性ビットを `MapFlags` に戻す。
fn decode_flags(d: u64) -> MapFlags {
    let user = d & AP_EL0 != 0;
    let device = (d >> 2) & 0b111 == 0;
    MapFlags {
        write: d & AP_RO == 0,
        exec: if user { d & UXN == 0 } else { d & PXN == 0 },
        user,
        device,
    }
}

fn table_descriptor(pa: PhysAddr) -> u64 {
    (pa.0 & ADDR_MASK) | VALID | TABLE_OR_PAGE
}

fn is_valid(d: u64) -> bool {
    d & VALID != 0
}

fn is_table(d: u64, level: Level) -> bool {
    is_valid(d) && d & TABLE_OR_PAGE != 0 && level != Level::L3
}

/// テーブルを置くメモリ。カーネルではフレーム割り当て器と線形マップ、テストでは偽物。
pub trait TableMemory {
    /// 0 で埋めた 4KiB のテーブルを1枚用意する。
    fn alloc_table(&mut self) -> Option<PhysAddr>;
    /// 物理アドレス `pa` のテーブルを読み書きする。
    fn table(&mut self, pa: PhysAddr) -> &mut [u64; ENTRIES];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    AlreadyMapped,
    Misaligned,
    OutOfMemory,
}

/// 1つのアドレス空間のページテーブル(L0 テーブルの物理アドレス)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageTable {
    root: PhysAddr,
}

impl PageTable {
    pub fn new(mem: &mut impl TableMemory) -> Result<Self, MapError> {
        let root = mem.alloc_table().ok_or(MapError::OutOfMemory)?;
        Ok(Self { root })
    }

    /// 既にあるテーブルを使う。
    pub const fn from_root(root: PhysAddr) -> Self {
        Self { root }
    }

    /// TTBR に書く値(L0 テーブルの物理アドレス)。
    pub fn root(&self) -> PhysAddr {
        self.root
    }

    /// `level` のテーブルまで降りる(途中のテーブルは必要なら作る)。
    fn descend(
        &self,
        mem: &mut impl TableMemory,
        va: VirtAddr,
        target: Level,
    ) -> Result<PhysAddr, MapError> {
        let mut table = self.root;
        for level in [Level::L0, Level::L1, Level::L2] {
            if level == target {
                break;
            }
            let i = level.index(va);
            let d = mem.table(table)[i];
            table = if is_table(d, level) {
                PhysAddr(d & ADDR_MASK)
            } else if is_valid(d) {
                return Err(MapError::AlreadyMapped); // 大きなブロックで既に覆われている
            } else {
                let next = mem.alloc_table().ok_or(MapError::OutOfMemory)?;
                mem.table(table)[i] = table_descriptor(next);
                next
            };
        }
        Ok(table)
    }

    fn map_leaf(
        &self,
        mem: &mut impl TableMemory,
        va: VirtAddr,
        pa: PhysAddr,
        flags: MapFlags,
        level: Level,
    ) -> Result<(), MapError> {
        let size = level.block_size();
        if !va.0.is_multiple_of(size) || !pa.0.is_multiple_of(size) {
            return Err(MapError::Misaligned);
        }
        let table = self.descend(mem, va, level)?;
        let entry = &mut mem.table(table)[level.index(va)];
        if is_valid(*entry) {
            return Err(MapError::AlreadyMapped);
        }
        *entry = leaf_descriptor(pa, flags, level);
        Ok(())
    }

    /// 4KiB ページを1枚マップする。
    pub fn map_page(
        &self,
        mem: &mut impl TableMemory,
        va: VirtAddr,
        pa: PhysAddr,
        flags: MapFlags,
    ) -> Result<(), MapError> {
        self.map_leaf(mem, va, pa, flags, Level::L3)
    }

    /// `len` バイトをマップする。そろっている所は 2MiB ブロック、それ以外は 4KiB ページを使う。
    pub fn map_range(
        &self,
        mem: &mut impl TableMemory,
        va: VirtAddr,
        pa: PhysAddr,
        len: u64,
        flags: MapFlags,
    ) -> Result<(), MapError> {
        let block = Level::L2.block_size();
        let mut off = 0;
        while off < len {
            let (v, p) = (va + off, pa + off);
            if v.0 % block == 0 && p.0 % block == 0 && len - off >= block {
                self.map_leaf(mem, v, p, flags, Level::L2)?;
                off += block;
            } else {
                self.map_leaf(mem, v, p, flags, Level::L3)?;
                off += PAGE_SIZE;
            }
        }
        Ok(())
    }

    /// 仮想アドレスを物理アドレスに変換する(CPU がするのと同じ手順をソフトウェアで)。
    pub fn translate(&self, mem: &mut impl TableMemory, va: VirtAddr) -> Option<PhysAddr> {
        let mut table = self.root;
        for level in [Level::L0, Level::L1, Level::L2, Level::L3] {
            let d = mem.table(table)[level.index(va)];
            if !is_valid(d) {
                return None;
            }
            if is_table(d, level) {
                table = PhysAddr(d & ADDR_MASK);
                continue;
            }
            let size = level.block_size();
            return Some(PhysAddr(
                (d & ADDR_MASK & !(size - 1)) | (va.0 & (size - 1)),
            ));
        }
        None
    }

    /// 変換し、記述子の属性も返す。
    pub fn translate_with_flags(
        &self,
        mem: &mut impl TableMemory,
        va: VirtAddr,
    ) -> Option<(PhysAddr, MapFlags)> {
        let mut table = self.root;
        for level in [Level::L0, Level::L1, Level::L2, Level::L3] {
            let d = mem.table(table)[level.index(va)];
            if !is_valid(d) {
                return None;
            }
            if is_table(d, level) {
                table = PhysAddr(d & ADDR_MASK);
                continue;
            }
            let size = level.block_size();
            let pa = PhysAddr((d & ADDR_MASK & !(size - 1)) | (va.0 & (size - 1)));
            return Some((pa, decode_flags(d)));
        }
        None
    }

    /// このアドレス空間のテーブル(L0〜L3)のフレームをすべて `release` に渡す。
    /// 末端のページが指すフレームは渡さない(持ち主が別に解放する)。
    pub fn free_tables(self, mem: &mut impl TableMemory, mut release: impl FnMut(PhysAddr)) {
        fn walk(
            mem: &mut impl TableMemory,
            table: PhysAddr,
            level: Level,
            release: &mut impl FnMut(PhysAddr),
        ) {
            let next = match level {
                Level::L0 => Some(Level::L1),
                Level::L1 => Some(Level::L2),
                Level::L2 => Some(Level::L3),
                Level::L3 => None,
            };
            if let Some(next) = next {
                for i in 0..ENTRIES {
                    let d = mem.table(table)[i];
                    if is_table(d, level) {
                        walk(mem, PhysAddr(d & ADDR_MASK), next, release);
                    }
                }
            }
            release(table);
        }
        walk(mem, self.root, Level::L0, &mut release);
    }

    /// 4KiB ページのマッピングを外し、指していたフレームを返す。
    /// TLB(変換のキャッシュ)の無効化は呼び出し元が行うこと。
    pub fn unmap_page(&self, mem: &mut impl TableMemory, va: VirtAddr) -> Option<PhysAddr> {
        let mut table = self.root;
        for level in [Level::L0, Level::L1, Level::L2] {
            let d = mem.table(table)[level.index(va)];
            if !is_table(d, level) {
                return None;
            }
            table = PhysAddr(d & ADDR_MASK);
        }
        let entry = &mut mem.table(table)[Level::L3.index(va)];
        if !is_valid(*entry) {
            return None;
        }
        let pa = PhysAddr(*entry & ADDR_MASK);
        *entry = 0;
        Some(pa)
    }
}

/// 線形マップを通してテーブルを読み書きする `TableMemory`(カーネルで使う)。
/// `alloc` は空きフレームを1枚返す関数(カーネルのフレーム割り当て器)。
pub struct LinearTableMemory<F: FnMut() -> Option<PhysAddr>> {
    alloc: F,
}

impl<F: FnMut() -> Option<PhysAddr>> LinearTableMemory<F> {
    /// # Safety
    /// `alloc` が返すフレームは RAM にあり、線形マップ(`phys_to_virt`)で読み書きできること。
    pub unsafe fn new(alloc: F) -> Self {
        Self { alloc }
    }
}

impl<F: FnMut() -> Option<PhysAddr>> TableMemory for LinearTableMemory<F> {
    fn alloc_table(&mut self) -> Option<PhysAddr> {
        let pa = (self.alloc)()?;
        self.table(pa).fill(0);
        Some(pa)
    }

    fn table(&mut self, pa: PhysAddr) -> &mut [u64; ENTRIES] {
        // SAFETY: new の契約により、pa は線形マップで見える RAM のフレーム
        unsafe { &mut *(phys_to_virt(pa).0 as *mut [u64; ENTRIES]) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hal::addr::{PhysAddr, VirtAddr};
    use hal::mmu::MapFlags;
    use std::boxed::Box;
    use std::collections::BTreeMap;

    /// 物理メモリに見立てたテーブルの置き場。
    struct FakeMemory {
        tables: BTreeMap<u64, Box<[u64; ENTRIES]>>,
        next: u64,
    }

    impl FakeMemory {
        fn new() -> Self {
            Self {
                tables: BTreeMap::new(),
                next: 0x8000_0000,
            }
        }
    }

    impl TableMemory for FakeMemory {
        fn alloc_table(&mut self) -> Option<PhysAddr> {
            let pa = self.next;
            self.next += 4096;
            self.tables.insert(pa, Box::new([0; ENTRIES]));
            Some(PhysAddr(pa))
        }
        fn table(&mut self, pa: PhysAddr) -> &mut [u64; ENTRIES] {
            self.tables.get_mut(&pa.0).expect("table exists")
        }
    }

    fn new_table() -> (FakeMemory, PageTable) {
        let mut mem = FakeMemory::new();
        let pt = PageTable::new(&mut mem).unwrap();
        (mem, pt)
    }

    // ---- 記述子 ----

    #[test]
    fn kernel_data_page_descriptor() {
        // ページ(0b11)、AttrIdx=1(Normal)、AP=00(EL1 RW)、SH=11、AF、PXN、UXN
        let d = leaf_descriptor(PhysAddr(0x4008_0000), MapFlags::KERNEL_RW, Level::L3);
        assert_eq!(
            d,
            0x4008_0000 | 0b11 | (1 << 2) | (0b11 << 8) | (1 << 10) | PXN | UXN
        );
    }

    #[test]
    fn kernel_code_is_executable_only_by_el1() {
        let d = leaf_descriptor(PhysAddr(0x1000), MapFlags::KERNEL_RX, Level::L3);
        assert_eq!(d & (PXN | UXN), UXN);
        assert_eq!((d >> 6) & 0b11, 0b10); // AP=10: EL1 読み出し専用
    }

    #[test]
    fn user_code_is_executable_only_by_el0_and_not_global() {
        let d = leaf_descriptor(PhysAddr(0x1000), MapFlags::USER_RX, Level::L3);
        assert_eq!(d & (PXN | UXN), PXN);
        assert_eq!((d >> 6) & 0b11, 0b11); // AP=11: EL0/EL1 読み出し専用
        assert_ne!(d & NG, 0);
    }

    #[test]
    fn user_data_is_writable_from_el0() {
        let d = leaf_descriptor(PhysAddr(0x1000), MapFlags::USER_RW, Level::L3);
        assert_eq!((d >> 6) & 0b11, 0b01);
    }

    #[test]
    fn device_memory_uses_attr0_and_is_never_executable() {
        let d = leaf_descriptor(PhysAddr(0x0900_0000), MapFlags::DEVICE, Level::L3);
        assert_eq!((d >> 2) & 0b111, 0);
        assert_eq!(d & (PXN | UXN), PXN | UXN);
    }

    #[test]
    fn block_descriptor_has_block_type_bits() {
        let d = leaf_descriptor(PhysAddr(0x4000_0000), MapFlags::KERNEL_RW, Level::L2);
        assert_eq!(d & 0b11, 0b01);
    }

    // ---- マッピング ----

    #[test]
    fn mapped_page_translates_with_offset() {
        let (mut mem, pt) = new_table();
        pt.map_page(
            &mut mem,
            VirtAddr(0x40_0000),
            PhysAddr(0x4100_0000),
            MapFlags::USER_RW,
        )
        .unwrap();
        assert_eq!(
            pt.translate(&mut mem, VirtAddr(0x40_0123)),
            Some(PhysAddr(0x4100_0123))
        );
    }

    #[test]
    fn intermediate_tables_are_created_once_per_region() {
        let (mut mem, pt) = new_table();
        pt.map_page(
            &mut mem,
            VirtAddr(0x40_0000),
            PhysAddr(0x1000),
            MapFlags::USER_RW,
        )
        .unwrap();
        assert_eq!(mem.tables.len(), 4); // L0 + L1 + L2 + L3
        pt.map_page(
            &mut mem,
            VirtAddr(0x40_1000),
            PhysAddr(0x2000),
            MapFlags::USER_RW,
        )
        .unwrap();
        assert_eq!(mem.tables.len(), 4);
    }

    #[test]
    fn mapping_twice_is_an_error() {
        let (mut mem, pt) = new_table();
        let va = VirtAddr(0x40_0000);
        pt.map_page(&mut mem, va, PhysAddr(0x1000), MapFlags::USER_RW)
            .unwrap();
        assert_eq!(
            pt.map_page(&mut mem, va, PhysAddr(0x2000), MapFlags::USER_RW),
            Err(MapError::AlreadyMapped)
        );
    }

    #[test]
    fn misaligned_addresses_are_rejected() {
        let (mut mem, pt) = new_table();
        assert_eq!(
            pt.map_page(
                &mut mem,
                VirtAddr(0x40_0001),
                PhysAddr(0x1000),
                MapFlags::USER_RW
            ),
            Err(MapError::Misaligned)
        );
    }

    #[test]
    fn unmapped_address_does_not_translate() {
        let (mut mem, pt) = new_table();
        assert_eq!(pt.translate(&mut mem, VirtAddr(0x1234_5000)), None);
    }

    #[test]
    fn unmap_returns_the_frame_and_removes_the_mapping() {
        let (mut mem, pt) = new_table();
        let va = VirtAddr(0x40_0000);
        pt.map_page(&mut mem, va, PhysAddr(0x5000), MapFlags::USER_RW)
            .unwrap();
        assert_eq!(pt.unmap_page(&mut mem, va), Some(PhysAddr(0x5000)));
        assert_eq!(pt.translate(&mut mem, va), None);
    }

    #[test]
    fn map_range_uses_2mib_blocks_where_possible() {
        let (mut mem, pt) = new_table();
        // 4KiB ずれた所から 4MiB + 8KiB:先頭と末尾は 4KiB ページ、中は 2MiB ブロック
        let va = VirtAddr(KERNEL_VBASE + 0x401f_f000);
        let pa = PhysAddr(0x401f_f000);
        pt.map_range(&mut mem, va, pa, 0x40_2000, MapFlags::KERNEL_RW)
            .unwrap();
        for off in [0u64, 0x1000, 0x20_0000, 0x3f_ffff, 0x40_1000] {
            assert_eq!(
                pt.translate(&mut mem, va + off),
                Some(pa + off),
                "offset {off:#x}"
            );
        }
        // L0 + L1 + L2 + 先頭の L3 + 末尾の L3
        assert_eq!(mem.tables.len(), 5);
    }

    #[test]
    fn linear_map_helpers() {
        assert_eq!(
            phys_to_virt(PhysAddr(0x0900_0000)),
            VirtAddr(0xffff_0000_0900_0000)
        );
        assert_eq!(
            virt_to_phys(VirtAddr(0xffff_0000_4008_0000)),
            PhysAddr(0x4008_0000)
        );
    }

    #[test]
    fn translate_with_flags_decodes_permissions() {
        let (mut mem, pt) = new_table();
        for (va, flags) in [
            (0x40_0000, MapFlags::USER_RX),
            (0x40_1000, MapFlags::USER_RW),
            (0x40_2000, MapFlags::USER_R),
            (0x40_3000, MapFlags::KERNEL_RW),
            (0x40_4000, MapFlags::DEVICE),
        ] {
            pt.map_page(&mut mem, VirtAddr(va), PhysAddr(0x9000), flags)
                .unwrap();
            assert_eq!(
                pt.translate_with_flags(&mut mem, VirtAddr(va + 8)),
                Some((PhysAddr(0x9008), flags)),
                "{va:#x}"
            );
        }
    }

    #[test]
    fn free_tables_returns_every_table_frame() {
        let (mut mem, pt) = new_table();
        pt.map_page(
            &mut mem,
            VirtAddr(0x40_0000),
            PhysAddr(0x1000),
            MapFlags::USER_RW,
        )
        .unwrap();
        pt.map_page(
            &mut mem,
            VirtAddr(0x7fff_0000_0000),
            PhysAddr(0x2000),
            MapFlags::USER_RW,
        )
        .unwrap();
        let tables: std::vec::Vec<u64> = mem.tables.keys().copied().collect();
        let mut freed = std::vec::Vec::new();
        pt.free_tables(&mut mem, |pa| freed.push(pa.0));
        freed.sort();
        assert_eq!(freed, tables);
    }
}
