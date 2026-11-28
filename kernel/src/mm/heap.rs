//! カーネルヒープ(`Box`、`Vec` などが使うメモリ)。
//!
//! 空きブロックを番地の順に並べた連結リストで管理する。
//! - 確保:先頭から順に見て、最初に収まるブロックを使う(first-fit)。余りは空きに戻す
//! - 解放:番地の順の位置に差し込み、前後の空きブロックと隣り合っていれば1つにまとめる
//!
//! 空きブロックの管理情報(大きさと次へのポインタ)は、空きブロック自身の中に書く。
//! そのため、確保の単位は管理情報が入る 16 バイト以上・16 バイト境界にする。

use crate::sync::SpinLock;
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::{self, NonNull};

const UNIT: usize = 16;

/// 空きブロックの先頭に書く管理情報。
struct FreeBlock {
    size: usize,
    next: Option<NonNull<FreeBlock>>,
}

const _: () = assert!(core::mem::size_of::<FreeBlock>() <= UNIT);

fn align_up(x: usize, align: usize) -> usize {
    (x + align - 1) & !(align - 1)
}

/// 確保の大きさと境界を、管理の単位にそろえる。
fn normalize(layout: Layout) -> (usize, usize) {
    (
        align_up(layout.size().max(UNIT), UNIT),
        layout.align().max(UNIT),
    )
}

pub struct Heap {
    head: Option<NonNull<FreeBlock>>,
    free: usize,
}

// SAFETY: Heap が指すメモリは Heap だけが管理する。共有するときはロックで守る
unsafe impl Send for Heap {}

impl Heap {
    pub const fn empty() -> Self {
        Self {
            head: None,
            free: 0,
        }
    }

    /// `start` から `size` バイトをヒープにする。
    ///
    /// # Safety
    /// その領域は有効なメモリで、ほかの用途に使われていないこと。`start` は 16 バイト境界。
    pub unsafe fn init(&mut self, start: usize, size: usize) {
        debug_assert_eq!(start % UNIT, 0);
        let size = size & !(UNIT - 1);
        let block = start as *mut FreeBlock;
        // SAFETY: 呼び出し元が領域の有効性を保証する
        unsafe { block.write(FreeBlock { size, next: None }) };
        self.head = NonNull::new(block);
        self.free = size;
    }

    pub fn free_bytes(&self) -> usize {
        self.free
    }

    /// 確保する。足りなければヌルポインタを返す。
    pub fn alloc(&mut self, layout: Layout) -> *mut u8 {
        let (size, align) = normalize(layout);
        let mut link: *mut Option<NonNull<FreeBlock>> = &mut self.head;
        // SAFETY: リストの各ブロックは init / dealloc で書いた有効な FreeBlock
        unsafe {
            while let Some(block) = *link {
                let start = block.as_ptr() as usize;
                let end = start + (*block.as_ptr()).size;
                let alloc_start = align_up(start, align);
                if alloc_start + size <= end {
                    let front = alloc_start - start;
                    let back = end - (alloc_start + size);
                    let mut rest = (*block.as_ptr()).next;
                    if back > 0 {
                        let tail = (alloc_start + size) as *mut FreeBlock;
                        tail.write(FreeBlock {
                            size: back,
                            next: rest,
                        });
                        rest = NonNull::new(tail);
                    }
                    if front > 0 {
                        // 境界合わせで余った前の部分は、空きブロックのまま残す
                        (*block.as_ptr()).size = front;
                        (*block.as_ptr()).next = rest;
                    } else {
                        *link = rest;
                    }
                    self.free -= size;
                    return alloc_start as *mut u8;
                }
                link = &mut (*block.as_ptr()).next;
            }
        }
        ptr::null_mut()
    }

    /// 解放する。
    ///
    /// # Safety
    /// `ptr` はこのヒープの `alloc` が同じ `layout` で返したもので、まだ解放していないこと。
    pub unsafe fn dealloc(&mut self, ptr: *mut u8, layout: Layout) {
        let (size, _) = normalize(layout);
        let addr = ptr as usize;
        // SAFETY: リストの各ブロックは有効な FreeBlock。ptr は呼び出し元が保証する
        unsafe {
            // addr より前にある最後の空きブロックを探す
            let mut prev: Option<NonNull<FreeBlock>> = None;
            let mut next = self.head;
            while let Some(block) = next {
                if block.as_ptr() as usize > addr {
                    break;
                }
                prev = next;
                next = (*block.as_ptr()).next;
            }
            let block = addr as *mut FreeBlock;
            block.write(FreeBlock { size, next });
            // 後ろの空きブロックと隣り合っていればまとめる
            if let Some(n) = next
                && addr + size == n.as_ptr() as usize
            {
                (*block).size += (*n.as_ptr()).size;
                (*block).next = (*n.as_ptr()).next;
            }
            // 前の空きブロックと隣り合っていればまとめる
            match prev {
                Some(p) if p.as_ptr() as usize + (*p.as_ptr()).size == addr => {
                    (*p.as_ptr()).size += (*block).size;
                    (*p.as_ptr()).next = (*block).next;
                }
                Some(p) => (*p.as_ptr()).next = NonNull::new(block),
                None => self.head = NonNull::new(block),
            }
        }
        self.free += size;
    }
}

/// ロック付きのヒープ。`#[global_allocator]` に使う。
/// 割り込みハンドラの中では確保も解放もしないこと(本体がロックを持っていると固まる)。
/// デバッグのビルドでは、ロックの検査がパニックで知らせる(ADR-0010)。
pub struct LockedHeap(SpinLock<Heap>);

impl LockedHeap {
    pub const fn empty() -> Self {
        Self(SpinLock::new(Heap::empty()))
    }

    /// # Safety
    /// [`Heap::init`] と同じ。
    pub unsafe fn init(&self, start: usize, size: usize) {
        // SAFETY: 呼び出し元が保証する
        unsafe { self.0.lock().init(start, size) };
    }

    pub fn free_bytes(&self) -> usize {
        self.0.lock().free_bytes()
    }
}

// SAFETY: Heap の操作はロックの中で行う
unsafe impl GlobalAlloc for LockedHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.0.lock().alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: GlobalAlloc の契約により、ptr はこのヒープが返したもの
        unsafe { self.0.lock().dealloc(ptr, layout) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::alloc::Layout;
    use std::vec::Vec;

    /// 16 バイト境界にそろえた試験用の領域
    #[repr(align(4096))]
    struct Arena([u8; 4096]);

    fn heap(arena: &mut Arena) -> Heap {
        let mut heap = Heap::empty();
        // SAFETY: arena はテストの間ずっと有効で、他から使われない
        unsafe { heap.init(arena.0.as_mut_ptr() as usize, arena.0.len()) };
        heap
    }

    fn layout(size: usize, align: usize) -> Layout {
        Layout::from_size_align(size, align).unwrap()
    }

    #[test]
    fn allocations_are_aligned_and_do_not_overlap() {
        let mut arena = Arena([0; 4096]);
        let mut h = heap(&mut arena);
        let a = h.alloc(layout(24, 8)) as usize;
        let b = h.alloc(layout(100, 16)) as usize;
        let c = h.alloc(layout(8, 64)) as usize;
        assert!(a != 0 && b != 0 && c != 0);
        assert_eq!(b % 16, 0);
        assert_eq!(c % 64, 0);
        let mut ranges = [(a, 24), (b, 100), (c, 8)];
        ranges.sort();
        for w in ranges.windows(2) {
            assert!(w[0].0 + w[0].1 <= w[1].0, "{ranges:x?}");
        }
    }

    #[test]
    fn freed_memory_is_reused() {
        let mut arena = Arena([0; 4096]);
        let mut h = heap(&mut arena);
        let a = h.alloc(layout(64, 16));
        // SAFETY: a は直前に同じ layout で確保した
        unsafe { h.dealloc(a, layout(64, 16)) };
        assert_eq!(h.alloc(layout(64, 16)), a);
    }

    #[test]
    fn exhausted_heap_returns_null() {
        let mut arena = Arena([0; 4096]);
        let mut h = heap(&mut arena);
        assert!(h.alloc(layout(8192, 16)).is_null());
    }

    #[test]
    fn adjacent_free_blocks_are_merged() {
        let mut arena = Arena([0; 4096]);
        let mut h = heap(&mut arena);
        // ほぼ全体を4つに分けて確保し、すべて解放すると、また全体を1つで確保できる
        let parts: Vec<_> = (0..4).map(|_| h.alloc(layout(1000, 16))).collect();
        assert!(parts.iter().all(|p| !p.is_null()));
        // 順番をばらばらにして解放する
        for i in [1, 3, 0, 2] {
            // SAFETY: 確保したものを1回ずつ解放する
            unsafe { h.dealloc(parts[i], layout(1000, 16)) };
        }
        assert_eq!(h.free_bytes(), 4096);
        assert!(!h.alloc(layout(4000, 16)).is_null());
    }

    #[test]
    fn free_bytes_tracks_usage() {
        let mut arena = Arena([0; 4096]);
        let mut h = heap(&mut arena);
        let p = h.alloc(layout(10, 1));
        assert_eq!(h.free_bytes(), 4096 - 16);
        // SAFETY: 確保したものを解放する
        unsafe { h.dealloc(p, layout(10, 1)) };
        assert_eq!(h.free_bytes(), 4096);
    }
}
