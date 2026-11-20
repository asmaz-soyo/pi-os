//! プロセスのユーザー空間のメモリ。
//!
//! カーネルはユーザーのポインタを直接読み書きしない。必ず [`UserMemory::copy_from_user`] /
//! [`UserMemory::copy_to_user`] を通す。これらはページテーブルで「ユーザーの領域にマップされているか」
//! 「書き込みが許されているか」を確かめてから、フレームの中身を(カーネルの線形マップ経由で)読み書きする。
//! そのため、ユーザーが変なポインタを渡しても、カーネルはフォールトせずに `EFAULT` を返せる。

use crate::Errno;
use alloc::boxed::Box;
use alloc::collections::BTreeSet;
use alloc::vec::Vec;
use hal::addr::{PAGE_SIZE, VirtAddr};
use hal::mmu::{AddressSpace, MapError, MapFlags, PhysMemory};

/// ユーザー空間の上限(48 ビットの下半分)。
pub const USER_END: u64 = 0x0001_0000_0000_0000;

pub struct UserMemory {
    space: Box<dyn AddressSpace>,
    phys: &'static dyn PhysMemory,
    /// マップしたページ(解放のため)
    pages: BTreeSet<u64>,
}

impl UserMemory {
    pub fn new(space: Box<dyn AddressSpace>, phys: &'static dyn PhysMemory) -> Self {
        Self {
            space,
            phys,
            pages: BTreeSet::new(),
        }
    }

    /// `va` から `len` バイトを 0 で埋めたページでマップする。既にマップされているページはそのまま使う。
    pub fn map_zeroed(&mut self, va: VirtAddr, len: u64, flags: MapFlags) -> Result<(), Errno> {
        let start = va.align_down();
        let end = (va + len).align_up();
        if end.0 > USER_END || end < start {
            return Err(Errno::EINVAL);
        }
        let mut page = start;
        while page < end {
            if !self.pages.contains(&page.0) {
                let frame = self.phys.alloc_frame().ok_or(Errno::ENOMEM)?;
                match self.space.map_page(page, frame, flags) {
                    Ok(()) => {
                        self.pages.insert(page.0);
                    }
                    Err(e) => {
                        self.phys.release_frame(frame);
                        return Err(match e {
                            MapError::OutOfMemory => Errno::ENOMEM,
                            _ => Errno::EINVAL,
                        });
                    }
                }
            }
            page = page + PAGE_SIZE;
        }
        Ok(())
    }

    /// `va` から `len` バイトのマッピングを外し、フレームを返す。
    pub fn unmap(&mut self, va: VirtAddr, len: u64) {
        let mut page = va.align_down();
        let end = (va + len).align_up();
        while page < end {
            if self.pages.remove(&page.0)
                && let Some(frame) = self.space.unmap_page(page)
            {
                self.phys.release_frame(frame);
            }
            page = page + PAGE_SIZE;
        }
    }

    pub fn is_mapped(&self, va: VirtAddr) -> bool {
        self.pages.contains(&va.align_down().0)
    }

    /// ユーザーのアドレス `va` を含むページの中身と、その中の位置。権限を確かめる。
    fn page_slice(&mut self, va: VirtAddr, write: bool) -> Result<(*mut u8, usize), Errno> {
        if va.0 >= USER_END {
            return Err(Errno::EFAULT);
        }
        let (pa, flags) = self.space.translate(va).ok_or(Errno::EFAULT)?;
        if !flags.user || (write && !flags.write) {
            return Err(Errno::EFAULT);
        }
        let page = hal::addr::PhysAddr(pa.0 & !(PAGE_SIZE - 1));
        let offset = (pa.0 & (PAGE_SIZE - 1)) as usize;
        // SAFETY: page はこのアドレス空間にマップされたフレーム
        Ok((unsafe { self.phys.frame_ptr(page) }, offset))
    }

    /// ページをまたぐ読み書きを、ページごとに分けて行う。
    fn each_chunk(
        &mut self,
        va: VirtAddr,
        len: usize,
        write: bool,
        mut f: impl FnMut(*mut u8, usize, usize),
    ) -> Result<(), Errno> {
        let mut done = 0;
        while done < len {
            let (base, offset) = self.page_slice(va + done as u64, write)?;
            let n = (PAGE_SIZE as usize - offset).min(len - done);
            // SAFETY: base は 4KiB のフレーム、offset + n <= 4KiB
            f(unsafe { base.add(offset) }, done, n);
            done += n;
        }
        Ok(())
    }

    /// ユーザーの `va` から `buf.len()` バイトを読む。
    pub fn copy_from_user(&mut self, va: VirtAddr, buf: &mut [u8]) -> Result<(), Errno> {
        let len = buf.len();
        self.each_chunk(va, len, false, |src, done, n| {
            // SAFETY: src は n バイト読めるフレームの中
            unsafe { core::ptr::copy_nonoverlapping(src, buf[done..].as_mut_ptr(), n) };
        })
    }

    /// ユーザーの `va` に `data` を書く。書き込みが許されたページだけ。
    pub fn copy_to_user(&mut self, va: VirtAddr, data: &[u8]) -> Result<(), Errno> {
        self.each_chunk(va, data.len(), true, |dst, done, n| {
            // SAFETY: dst は n バイト書けるフレームの中
            unsafe { core::ptr::copy_nonoverlapping(data[done..].as_ptr(), dst, n) };
        })
    }

    /// プログラムの読み込み用:読み出し専用のページにも書ける。
    pub fn load(&mut self, va: VirtAddr, data: &[u8]) -> Result<(), Errno> {
        self.each_chunk(va, data.len(), false, |dst, done, n| {
            // SAFETY: dst は n バイト書けるフレームの中(カーネルの線形マップ経由)
            unsafe { core::ptr::copy_nonoverlapping(data[done..].as_ptr(), dst, n) };
        })
    }

    /// NUL で終わる文字列を読む(NUL は含めない)。`max` バイト以内に NUL がなければ `ENAMETOOLONG`。
    pub fn read_cstr(&mut self, va: VirtAddr, max: usize) -> Result<Vec<u8>, Errno> {
        let mut out = Vec::new();
        let mut byte = [0u8; 1];
        while out.len() < max {
            self.copy_from_user(va + out.len() as u64, &mut byte)?;
            if byte[0] == 0 {
                return Ok(out);
            }
            out.push(byte[0]);
        }
        Err(Errno::ENAMETOOLONG)
    }

    /// このアドレス空間を CPU に設定する。
    pub fn activate(&mut self) {
        self.space.activate();
    }
}

impl Drop for UserMemory {
    fn drop(&mut self) {
        let pages: Vec<u64> = self.pages.iter().copied().collect();
        for page in pages {
            if let Some(frame) = self.space.unmap_page(VirtAddr(page)) {
                self.phys.release_frame(frame);
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::Errno;
    use hal::addr::{PAGE_SIZE, PhysAddr, VirtAddr};
    use hal::mmu::{AddressSpace, MapError, MapFlags, PhysMemory};
    use std::boxed::Box;
    use std::collections::BTreeMap;
    use std::vec::Vec;

    /// 偽の物理メモリ。フレームは Box で確保し、番号を物理アドレスに見立てる。
    pub struct FakePhys {
        frames: crate::sync::SpinLock<BTreeMap<u64, Box<[u8; 4096]>>>,
        next: core::sync::atomic::AtomicU64,
    }

    impl FakePhys {
        pub fn leak() -> &'static FakePhys {
            Box::leak(Box::new(FakePhys {
                frames: crate::sync::SpinLock::new(BTreeMap::new()),
                next: core::sync::atomic::AtomicU64::new(0x1000_0000),
            }))
        }
        pub fn live_frames(&self) -> usize {
            self.frames.lock().len()
        }
    }

    impl PhysMemory for FakePhys {
        fn alloc_frame(&self) -> Option<PhysAddr> {
            let pa = self
                .next
                .fetch_add(PAGE_SIZE, core::sync::atomic::Ordering::Relaxed);
            self.frames.lock().insert(pa, Box::new([0; 4096]));
            Some(PhysAddr(pa))
        }
        fn release_frame(&self, pa: PhysAddr) {
            self.frames.lock().remove(&pa.0).expect("double free");
        }
        unsafe fn frame_ptr(&self, pa: PhysAddr) -> *mut u8 {
            self.frames
                .lock()
                .get_mut(&pa.0)
                .expect("allocated frame")
                .as_mut_ptr()
        }
    }

    /// 偽のアドレス空間。
    #[derive(Default)]
    pub struct FakeSpace {
        pages: BTreeMap<u64, (PhysAddr, MapFlags)>,
    }

    impl AddressSpace for FakeSpace {
        fn map_page(
            &mut self,
            va: VirtAddr,
            pa: PhysAddr,
            flags: MapFlags,
        ) -> Result<(), MapError> {
            if self.pages.contains_key(&va.0) {
                return Err(MapError::AlreadyMapped);
            }
            self.pages.insert(va.0, (pa, flags));
            Ok(())
        }
        fn unmap_page(&mut self, va: VirtAddr) -> Option<PhysAddr> {
            self.pages.remove(&va.0).map(|(pa, _)| pa)
        }
        fn translate(&mut self, va: VirtAddr) -> Option<(PhysAddr, MapFlags)> {
            let page = va.align_down();
            self.pages
                .get(&page.0)
                .map(|&(pa, f)| (pa + (va - page), f))
        }
        fn activate(&mut self) {}
    }

    pub fn memory() -> (UserMemory, &'static FakePhys) {
        let phys = FakePhys::leak();
        (UserMemory::new(Box::new(FakeSpace::default()), phys), phys)
    }

    #[test]
    fn copy_round_trips_across_page_boundaries() {
        let (mut mem, _) = memory();
        mem.map_zeroed(VirtAddr(0x40_0000), 2 * PAGE_SIZE, MapFlags::USER_RW)
            .unwrap();
        let data: Vec<u8> = (0..100).collect();
        let va = VirtAddr(0x40_0000 + PAGE_SIZE - 50);
        mem.copy_to_user(va, &data).unwrap();
        let mut back = [0u8; 100];
        mem.copy_from_user(va, &mut back).unwrap();
        assert_eq!(&back[..], &data[..]);
    }

    #[test]
    fn new_memory_reads_as_zero() {
        let (mut mem, _) = memory();
        mem.map_zeroed(VirtAddr(0x40_0000), PAGE_SIZE, MapFlags::USER_RW)
            .unwrap();
        let mut buf = [0xffu8; 8];
        mem.copy_from_user(VirtAddr(0x40_0010), &mut buf).unwrap();
        assert_eq!(buf, [0; 8]);
    }

    #[test]
    fn unmapped_addresses_are_efault() {
        let (mut mem, _) = memory();
        let mut buf = [0u8; 4];
        assert_eq!(
            mem.copy_from_user(VirtAddr(0x1234), &mut buf),
            Err(Errno::EFAULT)
        );
        assert_eq!(
            mem.copy_to_user(VirtAddr(0x1234), b"hi"),
            Err(Errno::EFAULT)
        );
    }

    #[test]
    fn kernel_addresses_are_efault_even_if_mapped() {
        let (mut mem, _) = memory();
        let mut buf = [0u8; 4];
        assert_eq!(
            mem.copy_from_user(VirtAddr(0xffff_0000_4008_0000), &mut buf),
            Err(Errno::EFAULT)
        );
    }

    #[test]
    fn read_only_pages_reject_copy_to_user() {
        let (mut mem, _) = memory();
        mem.map_zeroed(VirtAddr(0x40_0000), PAGE_SIZE, MapFlags::USER_RX)
            .unwrap();
        assert_eq!(
            mem.copy_to_user(VirtAddr(0x40_0000), b"x"),
            Err(Errno::EFAULT)
        );
        // ローダーは読み出し専用のページにも中身を書ける
        mem.load(VirtAddr(0x40_0000), b"code").unwrap();
        let mut buf = [0u8; 4];
        mem.copy_from_user(VirtAddr(0x40_0000), &mut buf).unwrap();
        assert_eq!(&buf, b"code");
    }

    #[test]
    fn read_cstr_stops_at_nul_and_limits_length() {
        let (mut mem, _) = memory();
        mem.map_zeroed(VirtAddr(0x40_0000), PAGE_SIZE, MapFlags::USER_RW)
            .unwrap();
        mem.copy_to_user(VirtAddr(0x40_0000), b"/bin/qjs\0rest")
            .unwrap();
        assert_eq!(mem.read_cstr(VirtAddr(0x40_0000), 64).unwrap(), b"/bin/qjs");
        assert_eq!(
            mem.read_cstr(VirtAddr(0x40_0000), 4),
            Err(Errno::ENAMETOOLONG)
        );
    }

    #[test]
    fn mapping_over_an_existing_page_keeps_it() {
        let (mut mem, phys) = memory();
        mem.map_zeroed(VirtAddr(0x40_0000), PAGE_SIZE, MapFlags::USER_RX)
            .unwrap();
        mem.map_zeroed(VirtAddr(0x40_0000), 2 * PAGE_SIZE, MapFlags::USER_RW)
            .unwrap();
        assert_eq!(phys.live_frames(), 2);
    }

    #[test]
    fn dropping_the_memory_releases_all_frames() {
        let (mut mem, phys) = memory();
        mem.map_zeroed(VirtAddr(0x40_0000), 3 * PAGE_SIZE, MapFlags::USER_RW)
            .unwrap();
        assert_eq!(phys.live_frames(), 3);
        drop(mem);
        assert_eq!(phys.live_frames(), 0);
    }

    #[test]
    fn unmap_range_releases_frames() {
        let (mut mem, phys) = memory();
        mem.map_zeroed(VirtAddr(0x40_0000), 3 * PAGE_SIZE, MapFlags::USER_RW)
            .unwrap();
        mem.unmap(VirtAddr(0x40_1000), PAGE_SIZE);
        assert_eq!(phys.live_frames(), 2);
        assert!(!mem.is_mapped(VirtAddr(0x40_1000)));
        assert!(mem.is_mapped(VirtAddr(0x40_2000)));
    }
}
