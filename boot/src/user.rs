//! ユーザープロセスを動かすための配線。
//!
//! - [`KernelPhys`]:カーネルのフレーム割り当て器と線形マップを `hal::mmu::PhysMemory` として見せる
//! - [`UserSpace`]:AArch64 のページテーブルを `hal::mmu::AddressSpace` として見せる
//! - [`run_user`]:プログラムを読み込み、EL0 で動かし、終了ステータスを返す

use alloc::boxed::Box;
use alloc::sync::Arc;
use arch_aarch64::context;
use arch_aarch64::esr::{Esr, ExceptionClass};
use arch_aarch64::mmu::{LinearTableMemory, PageTable, phys_to_virt};
use arch_aarch64::trap::TrapFrame;
use hal::addr::{PAGE_SIZE, PhysAddr, VirtAddr};
use hal::mmu::{AddressSpace, MapError, MapFlags, PhysMemory};
use kernel::Errno;
use kernel::fs::devfs::{ConsoleFile, Devfs};
use kernel::fs::initramfs::Initramfs;
use kernel::fs::vfs::{File, Vfs};
use kernel::mm::user::UserMemory;
use kernel::process::{PROCESSES, Process};
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

/// タスクごとのユーザー空間のテーブル(TTBR0 に入れるもの)
static USER_TABLES: kernel::sync::SpinLock<
    alloc::collections::BTreeMap<kernel::sched::Tid, PhysAddr>,
> = kernel::sync::SpinLock::new(alloc::collections::BTreeMap::new());

/// タスクを切り替える直前に呼ばれる。次のタスクのユーザー空間を TTBR0 に入れる。
pub(crate) fn switch_address_space(next: kernel::sched::Tid) {
    let root = USER_TABLES.lock().get(&next).copied();
    let root = root.or(*EMPTY_USER_TABLE.lock());
    if let Some(root) = root {
        // SAFETY: 下位アドレスはユーザー空間専用で、カーネルは使っていない
        unsafe { arch_aarch64::cpu::set_user_table(root) };
    }
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
        // このタスクに切り替わるたびに、このテーブルを TTBR0 に入れる(switch_address_space)
        USER_TABLES
            .lock()
            .insert(kernel::sched::current(), self.table.root());
        // SAFETY: 下位アドレスはユーザー空間専用で、カーネルは使っていない
        unsafe { arch_aarch64::cpu::set_user_table(self.table.root()) };
    }
}

impl Drop for UserSpace {
    fn drop(&mut self) {
        // 使用中のテーブルを解放する前に、空のテーブルに切り替える
        let root = self.table.root();
        USER_TABLES.lock().retain(|_, r| *r != root);
        if let Some(empty) = *EMPTY_USER_TABLE.lock() {
            // SAFETY: 空のテーブルは何もマップしていない
            unsafe { arch_aarch64::cpu::set_user_table(empty) };
        }
        self.table.free_tables(&mut tables(), |pa| {
            kernel::mm::release_frame(pa).expect("table frame")
        });
    }
}

/// ユーザーのフォールトで終わったときの終了ステータス(128 + SIGSEGV)
pub const STATUS_SEGV: i32 = 128 + 11;

/// 今のプロセスを終える(ゾンビにしてタスクを終える)。
fn finish(status: i32) -> ! {
    PROCESSES.exit(kernel::sched::current(), status);
    kernel::sched::exit_current()
}

/// EL0 から来た同期例外(システムコールとフォールト)。
pub(crate) fn user_synchronous(frame: &mut TrapFrame) {
    match ExceptionClass::from_esr(frame.esr) {
        ExceptionClass::Svc64 => {
            let pid = kernel::sched::current();
            let x = &frame.x;
            // システムコールの間は、プロセスを表から取り出して使う(待っている間も表を使えるように)
            let mut process = PROCESSES.take(pid).expect("a user process is running");
            let action =
                kernel::syscall::dispatch(&mut process, x[8], [x[0], x[1], x[2], x[3], x[4], x[5]]);
            PROCESSES.put_back(pid, process);
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

/// EL0 に戻る直前(`before_return_to_user`)の、プロセスに関する処理。
pub(crate) fn before_return_to_user() {
    let pid = kernel::sched::current();
    // 保留中のシグナル(今は「強制終了」だけ)
    if let Some(Some(sig)) = PROCESSES.with(pid, |p| p.pending_kill.take()) {
        finish(128 + sig);
    }
}

/// 新しいプロセスのタスクの入口。アドレス空間を有効にして EL0 に入る。
fn user_task_entry(arg: usize) {
    // SAFETY: spawn_user が Box::into_raw で渡したもの
    let frame = unsafe { Box::from_raw(arg as *mut TrapFrame) };
    PROCESSES.with(kernel::sched::current(), |p| p.mem.activate());
    // SAFETY: アドレス空間は有効にした。このタスクのスタックで EL0 に入り、戻らない
    unsafe { context::enter_user_noreturn(&frame) }
}

/// プログラム `image` を `argv` で新しいプロセスとして動かし始める。
pub fn spawn_user(image: &[u8], argv: &[&[u8]]) -> Result<kernel::process::Pid, Errno> {
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
    let (vfs, console) = vfs();
    let process = Process::new(mem, loaded, vfs, console);

    // EL0t(SPSR.M = 0)、割り込みは許可した状態で入口から始める
    let frame = Box::new(TrapFrame {
        elr: loaded.entry,
        sp_el0: loaded.sp,
        spsr: 0,
        ..TrapFrame::default()
    });
    let arg = Box::into_raw(frame) as usize;
    let Some(pid) = kernel::sched::spawn(user_task_entry, arg) else {
        // SAFETY: 渡せなかったので取り戻して捨てる
        drop(unsafe { Box::from_raw(arg as *mut TrapFrame) });
        return Err(Errno::ENOMEM);
    };
    // 新しいタスクは、このタスクが譲るまで動かないので、ここで表に入れてよい
    PROCESSES.insert(pid, process);
    Ok(pid)
}

/// プロセスが終わるのを待ち、終了ステータスを返す。
pub fn wait(pid: kernel::process::Pid) -> i32 {
    loop {
        if let Some(status) = PROCESSES.try_wait(pid) {
            return status;
        }
        kernel::sched::yield_now();
    }
}

/// プロセスを強制終了する(次にユーザーに戻るときに、終了ステータス 128 + sig で終わる)。
pub fn kill(pid: kernel::process::Pid, sig: i32) {
    PROCESSES.with(pid, |p| p.pending_kill = Some(sig));
}

/// プログラム `image` を `argv` で実行し、終了ステータスを返す。
pub fn run_user(image: &[u8], argv: &[&[u8]]) -> Result<i32, Errno> {
    let pid = spawn_user(image, argv)?;
    Ok(wait(pid))
}

/// ルートに initramfs、`/dev` に devfs をマウントした VFS(最初の呼び出しで作る)。
fn vfs() -> (Arc<Vfs>, Arc<dyn File>) {
    static VFS: spin::Once<(Arc<Vfs>, Arc<ConsoleFile>)> = spin::Once::new();
    let (vfs, console) = VFS.call_once(|| {
        let root = Initramfs::new(userland::INITRAMFS).expect("initramfs is a valid tar");
        let console = Arc::new(ConsoleFile::new(Box::new(kernel::console::Global)));
        let mut vfs = Vfs::new(Arc::new(root));
        vfs.mount("/dev", Arc::new(Devfs::new(console.clone())));
        (Arc::new(vfs), console)
    });
    (vfs.clone(), console.clone())
}

/// initramfs のプログラム `path` を新しいプロセスとして動かし始める。
pub fn spawn_program(path: &str, argv: &[&[u8]]) -> Result<kernel::process::Pid, Errno> {
    let file = vfs().0.lookup(path)?;
    // init は root として動く(ADR-0005)。root でも実行ビットのないファイルは実行できない
    kernel::cred::Credentials::root().check(&file.stat(), kernel::cred::Access::EXEC)?;
    let image = file.contents().ok_or(Errno::EACCES)?;
    spawn_user(image, argv)
}

/// initramfs のプログラム `path` を実行し、終了ステータスを返す。
pub fn run_program(path: &str, argv: &[&[u8]]) -> Result<i32, Errno> {
    Ok(wait(spawn_program(path, argv)?))
}

/// initramfs のファイルの中身。
pub fn read_file(path: &str) -> Result<&'static [u8], Errno> {
    let file = vfs().0.lookup(path)?;
    let bytes = file.contents().ok_or(Errno::EACCES)?;
    // SAFETY: initramfs の中身は 'static な埋め込みデータ
    Ok(unsafe { core::slice::from_raw_parts(bytes.as_ptr(), bytes.len()) })
}
