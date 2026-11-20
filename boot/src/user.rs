//! ユーザープロセスを動かすための配線。
//!
//! - [`KernelPhys`]:カーネルのフレーム割り当て器と線形マップを `hal::mmu::PhysMemory` として見せる
//! - [`UserSpace`]:AArch64 のページテーブルを `hal::mmu::AddressSpace` として見せる
//! - [`run_user`]:プログラムを読み込み、EL0 で動かし、終了ステータスを返す

use alloc::boxed::Box;
use arch_aarch64::context::{self, KernelContext};
use arch_aarch64::esr::{Esr, ExceptionClass};
use arch_aarch64::mmu::{LinearTableMemory, PageTable, phys_to_virt};
use arch_aarch64::trap::TrapFrame;
use core::cell::UnsafeCell;
use hal::addr::{PAGE_SIZE, PhysAddr, VirtAddr};
use hal::mmu::{AddressSpace, MapError, MapFlags, PhysMemory};
use kernel::Errno;
use kernel::mm::user::UserMemory;
use kernel::process::{CURRENT, Process};
use kernel::syscall::Action;

/// カーネルのフレーム割り当て器(物理メモリ)。
pub struct KernelPhys;

pub static KERNEL_PHYS: KernelPhys = KernelPhys;

impl PhysMemory for KernelPhys {
    fn alloc_frame(&self) -> Option<PhysAddr> {
        let pa = kernel::mm::alloc_frame().ok()?;
        // SAFETY: 割り当てたばかりのフレーム
        unsafe { core::ptr::write_bytes(self.frame_ptr(pa), 0, PAGE_SIZE as usize) };
        Some(pa)
    }

    fn release_frame(&self, pa: PhysAddr) {
        kernel::mm::release_frame(pa).expect("frame was allocated");
    }

    unsafe fn frame_ptr(&self, pa: PhysAddr) -> *mut u8 {
        phys_to_virt(pa).0 as *mut u8
    }
}

fn table_frame() -> Option<PhysAddr> {
    kernel::mm::alloc_frame().ok()
}

fn tables() -> LinearTableMemory<fn() -> Option<PhysAddr>> {
    // SAFETY: フレームは RAM から取るので、線形マップで見える
    unsafe { LinearTableMemory::new(table_frame as fn() -> Option<PhysAddr>) }
}

/// プロセスがないときに TTBR0 に入れておく空のテーブル(`init_memory` で作る)。
pub(crate) static EMPTY_USER_TABLE: kernel::sync::SpinLock<Option<PhysAddr>> =
    kernel::sync::SpinLock::new(None);

/// プロセスのユーザー空間のページテーブル。
pub struct UserSpace {
    table: PageTable,
}

impl UserSpace {
    pub fn new() -> Result<Self, Errno> {
        let table = PageTable::new(&mut tables()).map_err(|_| Errno::ENOMEM)?;
        Ok(Self { table })
    }
}

impl AddressSpace for UserSpace {
    fn map_page(&mut self, va: VirtAddr, pa: PhysAddr, flags: MapFlags) -> Result<(), MapError> {
        self.table
            .map_page(&mut tables(), va, pa, flags)
            .map_err(|e| match e {
                arch_aarch64::mmu::MapError::AlreadyMapped => MapError::AlreadyMapped,
                arch_aarch64::mmu::MapError::Misaligned => MapError::Misaligned,
                arch_aarch64::mmu::MapError::OutOfMemory => MapError::OutOfMemory,
            })
    }

    fn unmap_page(&mut self, va: VirtAddr) -> Option<PhysAddr> {
        let pa = self.table.unmap_page(&mut tables(), va)?;
        arch_aarch64::cpu::flush_tlb_page(va);
        Some(pa)
    }

    fn translate(&mut self, va: VirtAddr) -> Option<(PhysAddr, MapFlags)> {
        self.table.translate_with_flags(&mut tables(), va)
    }

    fn activate(&mut self) {
        // SAFETY: 下位アドレスはユーザー空間専用で、カーネルは使っていない
        unsafe { arch_aarch64::cpu::set_user_table(self.table.root()) };
    }
}

impl Drop for UserSpace {
    fn drop(&mut self) {
        // 使用中のテーブルを解放する前に、空のテーブルに切り替える
        if let Some(empty) = *EMPTY_USER_TABLE.lock() {
            // SAFETY: 空のテーブルは何もマップしていない
            unsafe { arch_aarch64::cpu::set_user_table(empty) };
        }
        self.table.free_tables(&mut tables(), |pa| {
            kernel::mm::release_frame(pa).expect("table frame")
        });
    }
}

/// `run_user` が保存するカーネルの文脈。1つのコアで1つのプロセスだけを動かすので1つでよい。
struct ContextCell(UnsafeCell<KernelContext>);
// SAFETY: フェーズ1ではコア 0 だけが使う
unsafe impl Sync for ContextCell {}
static KERNEL_CONTEXT: ContextCell = ContextCell(UnsafeCell::new(KernelContext::new()));

/// ユーザーのフォールトで終わったときの終了ステータス(128 + SIGSEGV)
pub const STATUS_SEGV: i32 = 128 + 11;

/// プロセスを終えて `run_user` に戻る。
fn finish(status: i32) -> ! {
    // SAFETY: run_user が enter_user で保存した文脈に戻る
    unsafe { context::resume(KERNEL_CONTEXT.0.get(), status as u64) }
}

/// EL0 から来た同期例外(システムコールとフォールト)。
pub(crate) fn user_synchronous(frame: &mut TrapFrame) {
    match ExceptionClass::from_esr(frame.esr) {
        ExceptionClass::Svc64 => {
            let x = &frame.x;
            let action = {
                let mut current = CURRENT.lock();
                let process = current.as_mut().expect("a user process is running");
                kernel::syscall::dispatch(process, x[8], [x[0], x[1], x[2], x[3], x[4], x[5]])
            };
            match action {
                Action::Return(value) => frame.x[0] = value,
                Action::Exit(status) => finish(status),
            }
        }
        _ => {
            kernel::println!(
                "[pi-os] user fault: {} at pc {:#x}, address {:#x}",
                Esr(frame.esr),
                frame.elr,
                frame.far
            );
            finish(STATUS_SEGV)
        }
    }
}

/// プログラム `image` を `argv` で実行し、終了ステータスを返す。
pub fn run_user(image: &[u8], argv: &[&[u8]]) -> Result<i32, Errno> {
    let space = Box::new(UserSpace::new()?);
    let mut mem = UserMemory::new(space, &KERNEL_PHYS);
    let seed = arch_aarch64::cpu::counter();
    let mut random = [0u8; 16];
    random[..8].copy_from_slice(&seed.to_le_bytes());
    random[8..].copy_from_slice(
        &seed
            .rotate_left(29)
            .wrapping_mul(0x9e37_79b9_7f4a_7c15)
            .to_le_bytes(),
    );
    let loaded = kernel::exec::load(&mut mem, image, argv, &[], random)?;
    mem.activate();
    *CURRENT.lock() = Some(Process::new(mem, loaded, Box::new(kernel::console::Global)));

    // EL0t(SPSR.M = 0)、割り込みは許可した状態で入口から始める
    let frame = TrapFrame {
        elr: loaded.entry,
        sp_el0: loaded.sp,
        spsr: 0,
        ..TrapFrame::default()
    };
    // SAFETY: アドレス空間は有効にした。KERNEL_CONTEXT は static
    let status = unsafe { context::enter_user(KERNEL_CONTEXT.0.get(), &frame) } as i32;
    arch_aarch64::cpu::enable_irq();
    drop(CURRENT.lock().take());
    Ok(status)
}

/// initramfs のプログラム `path` を実行する。
pub fn run_program(path: &str, argv: &[&[u8]]) -> Result<i32, Errno> {
    let image = kernel::fs::tar::find(userland::INITRAMFS, path).ok_or(Errno::ENOENT)?;
    run_user(image, argv)
}
