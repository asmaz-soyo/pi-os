//! 物理ページ(フレーム)の割り当て。
//!
//! 管理する RAM を 4KiB のフレームに区切り、フレームごとに参照カウントを持つ。
//! 0 なら空き、1 以上なら使用中。参照カウントは、後で fork のコピーオンライトで
//! 「同じフレームを複数のプロセスが共有している」ことを表すのに使う(今は入れ物)。

use crate::Errno;
pub use hal::addr::PAGE_SIZE;
use hal::addr::PhysAddr;

pub struct FrameAllocator<'a> {
    base: PhysAddr,
    refs: &'a mut [u16],
    /// 次に探し始める位置(毎回先頭から探さないため)
    hint: usize,
    free: usize,
}

impl<'a> FrameAllocator<'a> {
    /// `base` から `refs.len()` 枚のフレームを管理する。`refs` は 0 で埋まっていること。
    pub fn new(base: PhysAddr, refs: &'a mut [u16]) -> Self {
        debug_assert!(base.is_aligned());
        let free = refs.iter().filter(|&&r| r == 0).count();
        Self {
            base,
            refs,
            hint: 0,
            free,
        }
    }

    fn index(&self, frame: PhysAddr) -> Result<usize, Errno> {
        if !frame.is_aligned() || frame < self.base {
            return Err(Errno::EINVAL);
        }
        let index = ((frame - self.base) / PAGE_SIZE) as usize;
        if index < self.refs.len() {
            Ok(index)
        } else {
            Err(Errno::EINVAL)
        }
    }

    fn address(&self, index: usize) -> PhysAddr {
        self.base + index as u64 * PAGE_SIZE
    }

    /// 空きフレームを1枚取る(参照カウント 1)。
    pub fn alloc(&mut self) -> Result<PhysAddr, Errno> {
        let len = self.refs.len();
        let index = (0..len)
            .map(|i| (self.hint + i) % len)
            .find(|&i| self.refs[i] == 0)
            .ok_or(Errno::ENOMEM)?;
        self.refs[index] = 1;
        self.free -= 1;
        self.hint = (index + 1) % len;
        Ok(self.address(index))
    }

    /// 連続した `count` 枚の空きフレームを取る。
    pub fn alloc_contiguous(&mut self, count: usize) -> Result<PhysAddr, Errno> {
        let mut run = 0;
        for i in 0..self.refs.len() {
            run = if self.refs[i] == 0 { run + 1 } else { 0 };
            if run == count {
                let start = i + 1 - count;
                self.refs[start..=i].fill(1);
                self.free -= count;
                return Ok(self.address(start));
            }
        }
        Err(Errno::ENOMEM)
    }

    /// 使用中のフレームの参照を1つ増やす(共有するとき)。
    pub fn add_ref(&mut self, frame: PhysAddr) -> Result<(), Errno> {
        let i = self.index(frame)?;
        if self.refs[i] == 0 {
            return Err(Errno::EINVAL);
        }
        self.refs[i] += 1;
        Ok(())
    }

    /// 参照を1つ減らし、0 になったら空きに戻す。
    pub fn release(&mut self, frame: PhysAddr) -> Result<(), Errno> {
        let i = self.index(frame)?;
        if self.refs[i] == 0 {
            return Err(Errno::EINVAL);
        }
        self.refs[i] -= 1;
        if self.refs[i] == 0 {
            self.free += 1;
        }
        Ok(())
    }

    /// `start` から `len` バイトを使用中にする(カーネル自身や管理用の領域)。
    pub fn reserve(&mut self, start: PhysAddr, len: u64) -> Result<(), Errno> {
        let first = self.index(start.align_down())?;
        let last = self.index((start + (len - 1)).align_down())?;
        for r in &mut self.refs[first..=last] {
            if *r == 0 {
                self.free -= 1;
            }
            *r = r.saturating_add(1).max(1);
        }
        Ok(())
    }

    pub fn ref_count(&self, frame: PhysAddr) -> u16 {
        self.index(frame).map(|i| self.refs[i]).unwrap_or(0)
    }

    pub fn free_frames(&self) -> usize {
        self.free
    }

    pub fn total_frames(&self) -> usize {
        self.refs.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Errno;
    use hal::addr::PhysAddr;
    use std::vec;

    const BASE: PhysAddr = PhysAddr(0x4010_0000);

    fn allocator(frames: usize) -> FrameAllocator<'static> {
        let refs = vec![0u16; frames].leak();
        FrameAllocator::new(BASE, refs)
    }

    #[test]
    fn allocates_distinct_frames_from_the_base() {
        let mut fa = allocator(4);
        assert_eq!(fa.alloc(), Ok(BASE));
        assert_eq!(fa.alloc(), Ok(BASE + PAGE_SIZE));
        assert_eq!(fa.free_frames(), 2);
    }

    #[test]
    fn freed_frame_can_be_allocated_again() {
        let mut fa = allocator(2);
        let a = fa.alloc().unwrap();
        let _b = fa.alloc().unwrap();
        fa.release(a).unwrap();
        assert_eq!(fa.alloc(), Ok(a));
    }

    #[test]
    fn running_out_of_frames_is_enomem() {
        let mut fa = allocator(1);
        fa.alloc().unwrap();
        assert_eq!(fa.alloc(), Err(Errno::ENOMEM));
    }

    #[test]
    fn shared_frame_is_freed_only_when_the_last_reference_goes() {
        let mut fa = allocator(1);
        let a = fa.alloc().unwrap();
        fa.add_ref(a).unwrap();
        assert_eq!(fa.ref_count(a), 2);
        fa.release(a).unwrap();
        assert_eq!(fa.free_frames(), 0);
        fa.release(a).unwrap();
        assert_eq!(fa.free_frames(), 1);
    }

    #[test]
    fn releasing_a_free_or_foreign_frame_is_einval() {
        let mut fa = allocator(2);
        assert_eq!(fa.release(BASE), Err(Errno::EINVAL));
        assert_eq!(fa.release(PhysAddr(0x1000)), Err(Errno::EINVAL));
        assert_eq!(fa.release(BASE + 1), Err(Errno::EINVAL));
    }

    #[test]
    fn reserve_marks_frames_as_used() {
        let mut fa = allocator(4);
        fa.reserve(BASE, 2 * PAGE_SIZE).unwrap();
        assert_eq!(fa.alloc(), Ok(BASE + 2 * PAGE_SIZE));
    }

    #[test]
    fn alloc_contiguous_finds_a_free_run() {
        let mut fa = allocator(6);
        let _a = fa.alloc().unwrap();
        let b = fa.alloc().unwrap();
        let _c = fa.alloc().unwrap();
        fa.release(b).unwrap();
        // b の1枠だけでは足りないので、その先の連続した3枠を返す
        assert_eq!(fa.alloc_contiguous(3), Ok(BASE + 3 * PAGE_SIZE));
        assert_eq!(fa.alloc_contiguous(1), Ok(b));
        assert_eq!(fa.alloc_contiguous(1), Err(Errno::ENOMEM));
    }
}
