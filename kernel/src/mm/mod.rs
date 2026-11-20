//! メモリ管理。

pub mod frame;
pub mod layout;

use crate::Errno;
use crate::sync::SpinLock;
use frame::FrameAllocator;
use hal::addr::PhysAddr;

/// カーネル全体の物理ページ割り当て器。起動時に [`init_frames`] で用意する。
static FRAMES: SpinLock<Option<FrameAllocator<'static>>> = SpinLock::new(None);

/// 物理ページ割り当て器を登録する。
pub fn init_frames(allocator: FrameAllocator<'static>) {
    *FRAMES.lock() = Some(allocator);
}

/// フレームを1枚取る。
pub fn alloc_frame() -> Result<PhysAddr, Errno> {
    FRAMES.lock().as_mut().ok_or(Errno::ENOMEM)?.alloc()
}

/// 連続したフレームを `count` 枚取る。
pub fn alloc_frames(count: usize) -> Result<PhysAddr, Errno> {
    FRAMES
        .lock()
        .as_mut()
        .ok_or(Errno::ENOMEM)?
        .alloc_contiguous(count)
}

/// フレームの参照を1つ減らす(0 になったら空きに戻る)。
pub fn release_frame(frame: PhysAddr) -> Result<(), Errno> {
    FRAMES.lock().as_mut().ok_or(Errno::EINVAL)?.release(frame)
}

/// (空き, 全体)のフレーム数。
pub fn frame_stats() -> (usize, usize) {
    FRAMES
        .lock()
        .as_ref()
        .map(|f| (f.free_frames(), f.total_frames()))
        .unwrap_or((0, 0))
}
