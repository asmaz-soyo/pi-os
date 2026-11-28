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
use kernel::fs::vfs::{File, FileSystem, Vfs};
use kernel::mm::user::UserMemory;
use kernel::process::{ExitStatus, PROCESSES, Process};
use kernel::signal::{Disposition, SigAction};
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

const SIGSEGV: i32 = 11;
/// ユーザーのフォールトで終わったときの終了ステータス(128 + SIGSEGV)
pub const STATUS_SEGV: i32 = 128 + SIGSEGV;

/// 今のスレッドを終える(グループの最初のスレッドならゾンビにして)、タスクを終える。
fn finish(status: ExitStatus) -> ! {
    let pid = kernel::sched::current();
    // set_tid_address / CLONE_CHILD_CLEARTID の場所に 0 を書いて、そこで待っている
    // スレッド(pthread_join)を起こす
    let cleared = PROCESSES.with(pid, |p| {
        let addr = p.clear_child_tid;
        let zero = &0u32.to_le_bytes();
        (addr != 0 && p.mem.copy_to_user(VirtAddr(addr), zero).is_ok()).then(|| (p.mem.id(), addr))
    });
    if let Some(Some(key)) = cleared {
        kernel::futex::wake(key, 1);
    }
    PROCESSES.exit(pid, status);
    kernel::sched::exit_current()
}

/// fork と execve で使う、新しいユーザー空間を作る関数。
fn new_user_space() -> Result<Box<dyn AddressSpace>, Errno> {
    Ok(Box::new(UserSpace::new()?))
}

/// 子プロセス(かスレッド)を、親のトラップフレームの写し(x0 = 0)から動かし始める。
/// `tls` があれば子の TPIDR_EL0 に、`set_tid` があれば子のメモリのそこに子の番号を書く
fn start_child(
    mut child: Process,
    frame: &TrapFrame,
    sp: Option<u64>,
    tls: Option<u64>,
    set_tid: Option<u64>,
) -> Result<u64, Errno> {
    let mut child_frame = Box::new(frame.clone());
    child_frame.x[0] = 0; // 子には 0 を返す
    if let Some(sp) = sp {
        child_frame.sp_el0 = sp;
    }
    if let Some(tls) = tls {
        child_frame.tpidr_el0 = tls;
    }
    let arg = Box::into_raw(child_frame) as usize;
    let Some(pid) = kernel::sched::spawn_paused(user_task_entry, arg) else {
        // SAFETY: 渡せなかったので取り戻して捨てる
        drop(unsafe { Box::from_raw(arg as *mut TrapFrame) });
        return Err(Errno::EAGAIN);
    };
    child.pid = pid;
    if child.tgid == 0 {
        child.tgid = pid; // fork:新しいグループ
    }
    if let Some(addr) = set_tid {
        let _ = child.mem.copy_to_user(VirtAddr(addr), &pid.to_le_bytes());
    }
    // 優先度は親から受け継ぐ
    kernel::sched::set_priority(pid, child.rt_priority);
    PROCESSES.insert(pid, child);
    // 表に入れてから動かす(入口で自分のアドレス空間を表から探すので)
    kernel::sched::wake(pid);
    Ok(pid as u64)
}

/// EL0 から来た同期例外(システムコールとフォールト)。
pub(crate) fn user_synchronous(frame: &mut TrapFrame) {
    match ExceptionClass::from_esr(frame.esr) {
        ExceptionClass::Svc64 => {
            let pid = kernel::sched::current();
            let x = &frame.x;
            let (nr, orig_x0) = (x[8], x[0]);
            // システムコールの間は、プロセスを表から取り出して使う(待っている間も表を使えるように)
            let mut process = PROCESSES.take(pid).expect("a user process is running");
            let action =
                kernel::syscall::dispatch(&mut process, x[8], [x[0], x[1], x[2], x[3], x[4], x[5]]);
            // シグナルで中断された:あとでやり直すかもしれないので、番号と元の x0 を覚えておく
            if action == Action::Return(Errno::EINTR.as_syscall_return()) {
                process.restart = Some((nr, orig_x0));
            }
            PROCESSES.put_back(pid, process);
            match action {
                Action::Return(value) => frame.x[0] = value,
                Action::Exit(status) => finish(ExitStatus::Code(status)),
                Action::SigReturn => sigreturn(pid, frame),
                Action::ExitGroup(status) => {
                    let tgid = PROCESSES.with(pid, |p| p.tgid).unwrap_or(pid);
                    let status = ExitStatus::Code(status);
                    PROCESSES.exit_group(pid, tgid, status);
                    finish(status)
                }
                Action::Fork {
                    child,
                    sp,
                    tls,
                    set_tid,
                } => {
                    frame.x[0] = match start_child(*child, frame, sp, tls, set_tid) {
                        Ok(pid) => pid,
                        Err(e) => e.as_syscall_return(),
                    }
                }
                Action::Exec { entry, sp } => {
                    // 新しいプログラムの入口から、レジスタを空にして始める
                    PROCESSES.with(pid, |p| p.mem.activate());
                    *frame = TrapFrame {
                        elr: entry,
                        sp_el0: sp,
                        spsr: 0,
                        ..TrapFrame::default()
                    };
                }
            }
        }
        _ => {
            kernel::println!(
                "[pi-os] user fault: {} at pc {:#x}, address {:#x}",
                Esr(frame.esr),
                frame.elr,
                frame.far
            );
            finish(ExitStatus::Signal(SIGSEGV))
        }
    }
}

/// EL0 に戻る直前(`before_return_to_user`)の、プロセスに関する処理:保留中のシグナルを配る。
/// ハンドラを呼ぶものは 1 つだけ(残りは、ハンドラから戻ってユーザーに戻るときに)
pub(crate) fn before_return_to_user(frame: &mut TrapFrame) {
    let pid = kernel::sched::current();
    let Some(restart) = PROCESSES.with(pid, |p| p.restart.take()) else {
        return;
    };
    loop {
        let blocked = PROCESSES.with(pid, |p| p.sig_blocked).unwrap_or(0);
        let Some(sig) = PROCESSES.take_signal(pid, blocked) else {
            break;
        };
        let disposition = PROCESSES
            .with(pid, |p| p.sig_actions.lock().disposition(sig))
            .expect("checked above");
        match disposition {
            Disposition::Ignore => {}
            Disposition::Terminate => terminate(pid, sig),
            Disposition::Handle(act) => {
                // SA_RESTART:ハンドラから戻ったら、中断されたシステムコールをやり直す
                // (フレームに積む前に、pc を svc に、x0 を元に戻しておく)
                if let Some((nr, x0)) = restart
                    && act.flags & kernel::signal::SA_RESTART != 0
                    && kernel::signal::restartable(nr)
                {
                    rewind(frame, x0);
                }
                if !deliver(pid, frame, sig, act) {
                    // フレームを積めない(スタックが壊れている):Linux と同じく SIGSEGV で終わる
                    terminate(pid, kernel::signal::SIGSEGV);
                }
                return;
            }
        }
    }
    // ハンドラは動かなかった(シグナルが無視された、など):中断されたシステムコールは
    // そのままやり直す。rt_sigsuspend で差し替えたマスクは元に戻す
    if let Some((_, x0)) = restart {
        rewind(frame, x0);
    }
    PROCESSES.with(pid, |p| {
        if let Some(mask) = p.saved_mask.take() {
            p.set_sig_blocked(mask);
        }
    });
}

/// システムコールをやり直すように戻す:pc を svc の命令に(4 バイト前)、x0 を元の引数に
fn rewind(frame: &mut TrapFrame, x0: u64) {
    frame.elr -= 4;
    frame.x[0] = x0;
}

/// シグナルの既定の動作で、グループごと終わる(終了ステータスは 128 + 番号)
fn terminate(pid: kernel::process::Pid, sig: u32) -> ! {
    let tgid = PROCESSES.with(pid, |p| p.tgid).unwrap_or(pid);
    let status = ExitStatus::Signal(sig as i32);
    PROCESSES.exit_group(pid, tgid, status);
    finish(status)
}

/// ハンドラを呼ぶ:ユーザーのスタック(SA_ONSTACK なら別のスタック)にシグナルフレームを積み、
/// マスクを足して、ハンドラから始めるようにレジスタを変える
fn deliver(pid: kernel::process::Pid, frame: &mut TrapFrame, sig: u32, act: SigAction) -> bool {
    use arch_aarch64::signal::{FRAME_SIZE, SigInfo, Stack};
    use kernel::signal::{SA_NODEFER, SA_ONSTACK, SA_RESETHAND, bit};
    const SS_ONSTACK: i32 = 1;
    const SS_DISABLE: i32 = 2;
    let done = PROCESSES.with(pid, |p| {
        let (alt_sp, _, alt_size) = p.sig_altstack;
        let on_alt = alt_size != 0 && frame.sp_el0 > alt_sp && frame.sp_el0 <= alt_sp + alt_size;
        let top = if act.flags & SA_ONSTACK != 0 && alt_size != 0 && !on_alt {
            alt_sp + alt_size
        } else {
            frame.sp_el0
        };
        let stack = Stack {
            sp: alt_sp,
            flags: if alt_size == 0 {
                SS_DISABLE
            } else if on_alt {
                SS_ONSTACK
            } else {
                0
            },
            size: alt_size,
        };
        let info = SigInfo {
            signo: sig as i32,
            ..SigInfo::default()
        };
        let addr = arch_aarch64::signal::frame_address(top);
        let mut buf = Box::new([0u8; FRAME_SIZE]);
        // フレームに残すのは、ハンドラから戻ったときのマスク(rt_sigsuspend の中なら、差し替える前のもの)
        let restore_mask = p.saved_mask.take().unwrap_or(p.sig_blocked);
        arch_aarch64::signal::write_frame(&mut buf, frame, &info, restore_mask, &stack);
        p.mem.copy_to_user(VirtAddr(addr), &buf[..]).ok()?;
        // ハンドラの間は sa_mask と(SA_NODEFER でなければ)同じシグナルもマスクする
        let mut blocked = p.sig_blocked | act.mask;
        if act.flags & SA_NODEFER == 0 {
            blocked |= bit(sig);
        }
        p.set_sig_blocked(blocked);
        if act.flags & SA_RESETHAND != 0 {
            let _ = p.sig_actions.lock().set(sig, SigAction::default());
        }
        Some(addr)
    });
    match done.flatten() {
        Some(addr) => {
            arch_aarch64::signal::enter_handler(frame, addr, sig, act.handler, act.restorer);
            true
        }
        None => false,
    }
}

/// rt_sigreturn:スタックのシグナルフレームから、レジスタとマスクを戻す
fn sigreturn(pid: kernel::process::Pid, frame: &mut TrapFrame) {
    use arch_aarch64::signal::FRAME_SIZE;
    let mut buf = Box::new([0u8; FRAME_SIZE]);
    let read = PROCESSES.with(pid, |p| {
        p.mem.copy_from_user(VirtAddr(frame.sp_el0), &mut buf[..])
    });
    if !matches!(read, Some(Ok(()))) {
        terminate(pid, kernel::signal::SIGSEGV);
    }
    let mask = arch_aarch64::signal::read_frame(&buf, frame);
    PROCESSES.with(pid, |p| p.set_sig_blocked(mask));
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
/// `exe` は `image` のファイルの絶対パス(/proc/<pid>/exe。ファイルでなければ空)
pub fn spawn_user(
    image: &(impl kernel::exec::Image + ?Sized),
    argv: &[&[u8]],
    exe: &str,
) -> Result<kernel::process::Pid, Errno> {
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
    // 動的リンクなら、動的リンカ(PT_INTERP のパス)も読み込む
    let interp = match kernel::exec::interpreter(image)? {
        Some(path) => Some(vfs().0.lookup(&path)?),
        None => None,
    };
    let loaded =
        kernel::exec::load_with_interp(&mut mem, image, interp.as_deref(), argv, &[], random)?;
    let (vfs, console) = vfs();
    let mut process = Process::new(mem, loaded, vfs, console);
    process.new_space = new_user_space;
    let name = argv.first().map_or(&b""[..], |a| a);
    let name = name.rsplit(|&c| c == b'/').next().unwrap_or(name);
    process.name = alloc::string::String::from_utf8_lossy(name).into_owned();
    process.exe = alloc::string::String::from(exe);

    // EL0t(SPSR.M = 0)、割り込みは許可した状態で入口から始める
    let frame = Box::new(TrapFrame {
        elr: loaded.entry,
        sp_el0: loaded.sp,
        spsr: 0,
        ..TrapFrame::default()
    });
    let arg = Box::into_raw(frame) as usize;
    let Some(pid) = kernel::sched::spawn_paused(user_task_entry, arg) else {
        // SAFETY: 渡せなかったので取り戻して捨てる
        drop(unsafe { Box::from_raw(arg as *mut TrapFrame) });
        return Err(Errno::ENOMEM);
    };
    // 止めたまま作ったので、表に入れてから動かす(カーネルのプリエンプションがあるので、
    // 普通に作ると、表に入れる前に割り込みで切り替わって動き出すことがある)
    process.pid = pid;
    process.tgid = pid;
    PROCESSES.insert(pid, process);
    kernel::sched::wake(pid);
    Ok(pid)
}

/// プロセスが終わるのを待ち、終了ステータス(シグナルなら 128 + 番号)を返す。
pub fn wait(pid: kernel::process::Pid) -> i32 {
    let status = kernel::wait::wait_event(&[&kernel::process::CHILD_WQ], None, || {
        PROCESSES.try_wait(pid)
    });
    status.expect("no deadline").shell_code()
}

/// プロセスを強制終了する(次にユーザーに戻るときに、終了ステータス 128 + sig で終わる)。
pub fn kill(pid: kernel::process::Pid, sig: i32) {
    PROCESSES.send(pid, sig as u32);
}

/// プログラム `image` を `argv` で実行し、終了ステータスを返す。
pub fn run_user(image: &[u8], argv: &[&[u8]]) -> Result<i32, Errno> {
    let pid = spawn_user(image, argv, "")?;
    Ok(wait(pid))
}

/// VFS:ルート(`/`)、`/dev` に devfs、`/tmp` に tmpfs、`/proc` に procfs、`/boot` に FAT32。
///
/// ディスクの ext2 にユーザーランド(`/bin/sh`)があれば、それをルートにする。なければ
/// (ディスクがない Pi の実機など)カーネルに埋め込んだ initramfs をルートにする。
fn vfs() -> (Arc<Vfs>, Arc<dyn File>) {
    static VFS: spin::Once<(Arc<Vfs>, Arc<ConsoleFile>)> = spin::Once::new();
    let (vfs, console) = VFS.call_once(|| {
        let console = Arc::new(ConsoleFile::new(Box::new(kernel::console::Global)));
        let mut mounts = disk_filesystems();
        let root_at = mounts
            .iter()
            .position(|(_, at, fs)| *at == "/mnt" && fs.lookup("/bin/sh").is_ok());
        let mut vfs = match root_at {
            Some(i) => {
                let (dev, _, fs) = mounts.remove(i);
                kernel::println!("ext2: {dev} mounted on /");
                kernel::fs::record_mount(&dev, "/", "ext2");
                Vfs::new(fs)
            }
            None => {
                kernel::fs::record_mount("rootfs", "/", "rootfs");
                Vfs::new(Arc::new(
                    Initramfs::new(userland::INITRAMFS).expect("initramfs is a valid tar"),
                ))
            }
        };
        vfs.mount("/dev", Arc::new(Devfs::new(console.clone())));
        vfs.mount("/tmp", Arc::new(kernel::fs::tmpfs::Tmpfs::new()));
        vfs.mount("/proc", Arc::new(kernel::fs::procfs::Procfs::new()));
        // /proc/mounts に見せる記録(df などが読む)
        kernel::fs::record_mount("devtmpfs", "/dev", "devtmpfs");
        kernel::fs::record_mount("tmpfs", "/tmp", "tmpfs");
        kernel::fs::record_mount("proc", "/proc", "proc");
        for (dev, at, fs) in mounts {
            let (kind, fstype) = if at == "/boot" {
                ("fat32", "vfat")
            } else {
                ("ext2", "ext2")
            };
            kernel::println!("{kind}: {dev} mounted on {at}");
            kernel::fs::record_mount(&dev, at, fstype);
            vfs.mount(at, fs);
        }
        (Arc::new(vfs), console)
    });
    (vfs.clone(), console.clone())
}

/// `/dev/vda` の MBR を読み、パーティションのファイルシステムを開く(SD カードの
/// 「p1 = FAT32、p2 = ext2」と同じ形)。(デバイス名, マウントする場所, ファイルシステム) を返す。
/// FAT32 は `/boot`、ext2 は `/mnt`(ルートにできるなら、呼ぶ側がルートにする)。
fn disk_filesystems() -> alloc::vec::Vec<(alloc::string::String, &'static str, Arc<dyn FileSystem>)>
{
    use kernel::block::mbr;
    let mut out = alloc::vec::Vec::new();
    let Some(disk) = kernel::block::find("vda") else {
        return out;
    };
    let mut sector = [0u8; 512];
    if disk.lock().read_block(0, &mut sector).is_err() {
        return out;
    }
    let Some(parts) = mbr::parse(&sector) else {
        return out;
    };
    for (i, part) in parts.iter().enumerate() {
        let dev =
            kernel::block::PartitionDevice::new(disk.clone(), part.start as u64, part.count as u64);
        let dev = kernel::block::shared(Box::new(dev));
        let name = alloc::format!("/dev/vda{}", i + 1);
        let opened: Result<(&'static str, Arc<dyn FileSystem>), Errno> = match part.kind {
            mbr::KIND_FAT32_LBA => {
                kernel::fs::fat32::Fat32::new(dev).map(|fs| ("/boot", Arc::new(fs) as _))
            }
            mbr::KIND_LINUX => {
                kernel::fs::ext2::Ext2::new(dev).map(|fs| ("/mnt", Arc::new(fs) as _))
            }
            _ => continue,
        };
        match opened {
            Ok((at, fs)) => out.push((name, at, fs)),
            Err(e) => kernel::println!("cannot mount {name}: {e:?}"),
        }
    }
    out
}

/// マウントしているファイルシステムの、まだ本来の場所に書いていないもの(ジャーナルに
/// コミットしただけのもの)を書く
pub fn sync_filesystems() -> Result<(), Errno> {
    vfs().0.sync()
}

/// プログラム `path` を新しいプロセスとして動かし始める。
pub fn spawn_program(path: &str, argv: &[&[u8]]) -> Result<kernel::process::Pid, Errno> {
    let file = vfs().0.lookup(path)?;
    // init は root として動く(ADR-0005)。root でも実行ビットのないファイルは実行できない
    kernel::cred::Credentials::root().check(&file.stat(), kernel::cred::Access::EXEC)?;
    let exe = vfs().0.resolve_path(path, true)?;
    // ファイルから少しずつ読み込む(全体をカーネルのヒープに置かない)
    spawn_user(&*file, argv, &exe)
}

/// プログラム `path` を実行し、終了ステータスを返す。
pub fn run_program(path: &str, argv: &[&[u8]]) -> Result<i32, Errno> {
    Ok(wait(spawn_program(path, argv)?))
}

/// ファイルの中身(initramfs ならメモリをそのまま、それ以外は読んで返す)。
pub fn read_file(path: &str) -> Result<alloc::borrow::Cow<'static, [u8]>, Errno> {
    let file = vfs().0.lookup(path)?;
    kernel::exec::read_image(&*file)
}
