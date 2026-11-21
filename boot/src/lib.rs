//! AArch64 ボード共通の起動処理(初期化の順番)と、BSP が埋める `Board` トレイト。
//!
//! BSP は「どのドライバをどのアドレスで使うか」だけを [`Board`] として書き、
//! 初期化の順番やパニックの処理はここで共通にする。
#![no_std]

extern crate alloc;

mod user;

pub use user::{STATUS_SEGV, kill, run_program, run_user, spawn_program, spawn_user, wait};
pub use userland;

use arch_aarch64::exception::{self, Handler};
use arch_aarch64::mmu::{LinearTableMemory, PageTable, phys_to_virt};
use arch_aarch64::timer::GenericTimer;
use arch_aarch64::trap::TrapFrame;
use core::marker::PhantomData;
use core::ops::Range;
use hal::addr::{PAGE_SIZE, PhysAddr};
use hal::console::Console;
use hal::irq::IrqController;
use hal::timer::Timer;
use kernel::mm::frame::FrameAllocator;
use kernel::sync::SpinLock;

/// ボードごとの定数とデバイス。BSP が実装する。
pub trait Board: 'static {
    /// ボードの名前(起動メッセージに出る)
    const NAME: &'static str;
    /// カーネルが使ってよい RAM(物理アドレス)
    const RAM: Range<u64>;
    /// デバイスのレジスタがある範囲(物理アドレス)
    const DEVICES: &'static [Range<u64>];
    /// ARM Generic Timer の割り込み番号(割り込みコントローラでの番号)
    const TIMER_IRQ: u32;
    /// タイマ割り込みの頻度
    const TIMER_HZ: u64 = 100;
    /// virtio-mmio の枠の物理アドレス(ないボードは空)
    const VIRTIO_MMIO: &'static [usize] = &[];
    /// GPIO コントローラの割り込み番号(ないボードは None)
    const GPIO_IRQ: Option<u32> = None;
    /// GPIO の割り込みの要因(線のビット)を読んで消す
    fn gpio_take_pending() -> u32 {
        0
    }

    type Console: Console + Send;
    type Intc: IrqController + Send;

    fn console() -> &'static SpinLock<Self::Console>;
    fn intc() -> &'static SpinLock<Self::Intc>;
    /// UART と割り込みコントローラの初期化。MMU を有効にしたあとに呼ばれる。
    fn init_devices();
}

/// デバイスの物理アドレスを、カーネルから見える仮想アドレスにする(BSP の static の初期化用)。
pub const fn device_va(pa: usize) -> usize {
    phys_to_virt(PhysAddr(pa as u64)).0 as usize
}

/// 起動処理。`kernel_main` の最初に呼ぶ。順番に意味がある。
pub fn init<B: Board>() {
    // 1. 例外ベクタ(以降の不具合を例外として捕まえられるように)
    exception::init();
    // 2. 物理メモリとページテーブル(デバイスのアドレスが見えるようになる)
    init_memory::<B>();
    // 3. カーネルヒープ(ここから Box や Vec が使える)
    init_heap();
    // 4. UART と割り込みコントローラ。ここから println! が使える
    B::init_devices();
    kernel::console::set_console(B::console());
    probe_virtio::<B>();
    // 5. スケジューラ(ここまでの起動処理がタスク 0 になる)
    init_scheduler();
    // 6. タイマ割り込み
    start_timer::<B>();
    // 7. キャッシュに残った書き込みを、後ろで少しずつディスクへ書くスレッド(ディスクがあるときだけ)
    if kernel::block::find("vda").is_some() {
        kernel::sched::spawn(flusher, 0).expect("start the flusher thread");
    }
    // 8. ネットワーク:見つけたカードをスタックにつなぎ、後ろで poll するスレッドを動かす
    if bring_up_network() {
        kernel::sched::spawn(net_poller, 0).expect("start the network poller");
    }
    if cfg!(feature = "trace-syscalls") {
        kernel::syscall::TRACE.store(true, core::sync::atomic::Ordering::Relaxed);
    }
}

/// カーネルのイメージより後ろの RAM をフレーム割り当て器に渡し、
/// カーネルのページテーブルを作り直して切り替える。
fn init_memory<B: Board>() {
    let image = arch_aarch64::boot::kernel_image_phys();
    let start = image.end.align_up();
    let end = PhysAddr(B::RAM.end);
    let frames = ((end - start) / PAGE_SIZE) as usize;
    // 参照カウントの表は、管理する領域の先頭に置く
    // SAFETY: start 以降はカーネルのイメージの外の RAM で、起動用のマップで見えている
    let refs =
        unsafe { core::slice::from_raw_parts_mut(phys_to_virt(start).0 as *mut u16, frames) };
    refs.fill(0);
    let mut allocator = FrameAllocator::new(start, refs);
    allocator
        .reserve(start, (frames * 2) as u64)
        .expect("reference count table fits in RAM");
    kernel::mm::init_frames(allocator);

    // SAFETY: フレームは RAM から取るので、線形マップで見える
    let mut mem = unsafe { LinearTableMemory::new(|| kernel::mm::alloc_frame().ok()) };
    let kernel_table = PageTable::new(&mut mem).expect("frame for kernel table");
    let ram = B::RAM;
    let image_range = image.start.0..image.end.0;
    for m in kernel::mm::layout::kernel_mappings(ram, image_range, B::DEVICES) {
        kernel_table
            .map_range(&mut mem, phys_to_virt(m.pa), m.pa, m.len, m.flags)
            .expect("kernel mapping");
    }
    let user_table = PageTable::new(&mut mem).expect("frame for user table");
    *user::EMPTY_USER_TABLE.lock() = Some(user_table.root());
    // SAFETY: 新しいテーブルはカーネルのイメージとスタックを同じ仮想アドレスでマップしている。
    // 下位アドレス(恒等マップ)はもう使っていない
    unsafe {
        arch_aarch64::cpu::set_kernel_table(kernel_table.root());
        arch_aarch64::cpu::set_user_table(user_table.root());
    }
}

/// カーネルヒープの大きさ
const HEAP_SIZE: usize = 4 * 1024 * 1024;

/// 連続したフレームを取ってヒープにする。
fn init_heap() {
    let frames = HEAP_SIZE / PAGE_SIZE as usize;
    let start = kernel::mm::alloc_frames(frames).expect("contiguous frames for the heap");
    // SAFETY: 割り当てたばかりのフレームで、線形マップで見えている
    unsafe { kernel::mm::HEAP.init(phys_to_virt(start).0 as usize, HEAP_SIZE) };
}

/// カーネルスタックの大きさ(64KiB。デバッグビルドでも余裕を持たせる)
const KERNEL_STACK_PAGES: usize = 16;

fn alloc_kernel_stack() -> Option<(usize, usize)> {
    let pa = kernel::mm::alloc_frames(KERNEL_STACK_PAGES).ok()?;
    let bottom = phys_to_virt(pa).0 as usize;
    Some((bottom, bottom + KERNEL_STACK_PAGES * PAGE_SIZE as usize))
}

fn free_kernel_stack(bottom: usize) {
    let pa = arch_aarch64::mmu::virt_to_phys(hal::addr::VirtAddr(bottom as u64));
    for i in 0..KERNEL_STACK_PAGES {
        let _ = kernel::mm::release_frame(pa + (i as u64) * PAGE_SIZE);
    }
}

unsafe fn switch_context(
    from: *mut kernel::sched::SavedContext,
    to: *const kernel::sched::SavedContext,
) {
    // SAFETY: SavedContext は repr(C) の [u64; 16]。スケジューラが正しい文脈を渡す
    unsafe { arch_aarch64::context::switch(from.cast(), to.cast()) }
}

fn new_context(
    stack_top: usize,
    entry: fn(usize),
    arg: usize,
    exit: fn(),
) -> kernel::sched::SavedContext {
    kernel::sched::SavedContext {
        regs: arch_aarch64::context::new_thread_context(stack_top, entry, arg, exit),
    }
}

/// 汚れてから 5 秒たったセクタを、1 秒ごとにディスクへ書く(Linux の dirty_expire と同じ考え)。
/// 書き込みをしばらくまとめつつ、急に電源が切れても失うのは最後の数秒分だけにする
fn flusher(_: usize) {
    const SEC: u64 = 1_000_000_000;
    let mut tick = 0u64;
    loop {
        kernel::sched::sleep_until(kernel::time::now_ns() + SEC);
        tick += 1;
        // 5 秒ごとに、ジャーナルにコミットしただけのものを本来の場所に書く(チェックポイント)
        let checkpoint = if tick % 5 == 0 {
            user::sync_filesystems()
        } else {
            Ok(())
        };
        if let Err(e) = checkpoint.and_then(|_| kernel::block::flush_older_all(5 * SEC)) {
            kernel::println!("flusher: {e:?}");
        }
    }
}

/// eth0 をネットワークスタックにつなぐ。つないだら true。
/// アドレスは今は QEMU のユーザーモードのネットワークの決まった値(DHCP にするまでの仮)
fn bring_up_network() -> bool {
    use smoltcp::wire::{IpAddress, IpCidr, Ipv4Address};
    let Some(dev) = kernel::net::dev::take("eth0") else {
        return false;
    };
    kernel::net::inet::attach(
        dev,
        Some(IpCidr::new(IpAddress::v4(10, 0, 2, 15), 24)),
        Some(Ipv4Address::new(10, 0, 2, 2)),
    );
    true
}

/// だれもシステムコールで待っていないときに届いたパケットや、再送のタイマーのために、
/// ネットワークを poll する。smoltcp が「次はいつ poll が要るか」を教えてくれるので、それまで眠る
/// (急ぎがなければ 50ms。受信の割り込みを使うようになれば、届いたときに起こせる)
fn net_poller(_: usize) {
    const MIN: u64 = 1_000_000;
    const MAX: u64 = 50_000_000;
    let mut delay = MAX;
    loop {
        kernel::sched::sleep_until(kernel::time::now_ns() + delay.max(MIN));
        delay = kernel::net::inet::poll_all(MAX);
    }
}

fn init_scheduler() {
    kernel::sched::init(kernel::sched::Platform {
        switch: switch_context,
        new_context,
        alloc_stack: alloc_kernel_stack,
        free_stack: free_kernel_stack,
        irq_save: arch_aarch64::cpu::irq_save,
        irq_restore: arch_aarch64::cpu::irq_restore,
        before_switch: user::switch_address_space,
    });
}

/// virtio-mmio の枠を調べ、デバイスの種類ごとにドライバを付ける。
/// - virtio-blk(2):`vda`、`vdb`、… としてブロック層に登録する
/// - virtio-net(1):`eth0`、`eth1`、… としてネットワークデバイスに登録する
fn probe_virtio<B: Board>() {
    use hal::mmio::Mmio;
    let mut next_disk = b'a';
    for &pa in B::VIRTIO_MMIO {
        // SAFETY: pa は virtio-mmio の枠で、デバイスの範囲としてマップ済み
        let mut regs = unsafe { hal::mmio::MmioRegion::new(device_va(pa)) };
        if regs.read(virtio_blk::MAGIC) != 0x7472_6976 {
            continue;
        }
        match regs.read(virtio_blk::DEVICE_ID) {
            2 => {
                if probe_blk(pa, next_disk) {
                    next_disk += 1;
                }
            }
            1 => probe_net(pa),
            _ => {} // 空の枠(0)やまだ知らないデバイス
        }
    }
}

fn probe_blk(pa: usize, letter: u8) -> bool {
    let Ok(dma) = kernel::mm::alloc_frame() else {
        return false;
    };
    // SAFETY: pa は virtio-mmio の枠で、デバイスの範囲としてマップ済み
    let regs = unsafe { hal::mmio::MmioRegion::new(device_va(pa)) };
    let dma_va = phys_to_virt(dma).0 as *mut u8;
    // SAFETY: dma は割り当てたばかりのフレームで、このドライバだけが使う
    let Some(dev) = (unsafe { virtio_blk::VirtioBlk::new(regs, dma_va, dma.0) }) else {
        let _ = kernel::mm::release_frame(dma);
        return false;
    };
    let name = alloc::format!("vd{}", letter as char);
    kernel::println!("virtio-blk: /dev/{name}, {} sectors", dev.capacity());
    // 最近使った 256 セクタ(128KB)をメモリに残す。書き込みは sync するときと、
    // そのセクタを捨てるときにディスクへ(write-back)
    let cached = kernel::block::cache::CachedDevice::write_back(alloc::boxed::Box::new(dev), 256);
    kernel::block::register(&name, kernel::block::shared(alloc::boxed::Box::new(cached)));
    true
}

fn probe_net(pa: usize) {
    // リングに 1 ページ、送り受けのバッファに QUEUE_SIZE ページ
    let mut frames = alloc::vec::Vec::new();
    for _ in 0..=virtio_net::QUEUE_SIZE {
        match kernel::mm::alloc_frame() {
            Ok(f) => frames.push(f),
            Err(_) => {
                for f in frames {
                    let _ = kernel::mm::release_frame(f);
                }
                return;
            }
        }
    }
    let page = |f: hal::addr::PhysAddr| virtio_net::Page {
        va: phys_to_virt(f).0 as *mut u8,
        pa: f.0,
    };
    let ring = page(frames[0]);
    let bufs: [virtio_net::Page; virtio_net::QUEUE_SIZE] =
        core::array::from_fn(|i| page(frames[i + 1]));
    // SAFETY: pa は virtio-mmio の枠。ページは割り当てたばかりで、このドライバだけが使う
    let regs = unsafe { hal::mmio::MmioRegion::new(device_va(pa)) };
    match unsafe { virtio_net::VirtioNet::new(regs, ring, bufs) } {
        Some(dev) => {
            let m = hal::net::NetDev::mac(&dev);
            let name = kernel::net::dev::register(alloc::boxed::Box::new(dev));
            kernel::println!(
                "virtio-net: {name}, MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                m[0],
                m[1],
                m[2],
                m[3],
                m[4],
                m[5]
            );
        }
        None => {
            for f in frames {
                let _ = kernel::mm::release_frame(f);
            }
        }
    }
}

/// 例外の受け口。IRQ は割り込みコントローラから番号を読み、カーネルの表で振り分ける。
struct Exceptions<B>(PhantomData<fn() -> B>);

impl<B: Board> Handler for Exceptions<B> {
    fn synchronous(&self, frame: &mut TrapFrame) {
        if exception::from_user(frame) {
            user::user_synchronous(frame)
        } else {
            exception::unhandled(exception::KIND_SYNC_CURRENT_EL, frame)
        }
    }

    fn before_return_to_user(&self, _frame: &mut TrapFrame) {
        user::before_return_to_user();
        kernel::process::before_return_to_user();
    }

    fn irq(&self, _frame: &mut TrapFrame) {
        kernel::irq::IRQS.dispatch(&mut *B::intc().lock());
    }
}

fn gpio_irq<B: Board>() {
    kernel::gpio::GPIO.record(B::gpio_take_pending());
}

fn timer_tick() {
    // ティックでも目覚ましでも、ユーザーに戻るときに選び直す
    // (ほかに動けるタスク・起きる時刻になったタスクがあれば切り替わる。プリエンプション)
    kernel::time::CLOCK.tick(&mut GenericTimer);
    kernel::sched::request_resched();
}

fn start_timer<B: Board>() {
    let handler: &'static Exceptions<B> = &Exceptions(PhantomData);
    exception::set_handler(Some(handler));
    kernel::irq::IRQS
        .register(B::TIMER_IRQ, timer_tick)
        .expect("timer irq number is valid");
    B::intc().lock().enable(B::TIMER_IRQ);
    if let Some(irq) = B::GPIO_IRQ {
        kernel::irq::IRQS
            .register(irq, gpio_irq::<B>)
            .expect("gpio irq number is valid");
        B::intc().lock().enable(irq);
    }
    kernel::time::CLOCK.start(&mut GenericTimer, B::TIMER_HZ);
    kernel::time::set_alarm(|ns| kernel::time::CLOCK.request_wake_ns(&mut GenericTimer, ns));
    kernel::time::set_counter(arch_aarch64::cpu::counter, GenericTimer.frequency());
    arch_aarch64::cpu::enable_irq();
}

/// パニックの処理。BSP の `#[panic_handler]` から呼ぶ。
pub fn panic(info: &core::panic::PanicInfo) -> ! {
    let r = arch_aarch64::cpu::snapshot();
    let regs = kernel::panic::Registers {
        current_el: r.current_el,
        sp: r.sp,
        fp: r.fp,
        lr: r.lr,
    };
    kernel::panic::report(info, &regs, arch_aarch64::boot::stack_range());
    // QEMU なら終了コード 1 で終わる。実機ではここで止まる
    exit(1)
}

/// 終了する。
///
/// QEMU 用のビルド(`semihosting` feature)ならセミホスティングで終了コードを返す。
/// 実機にはその仕組みがないので、メッセージを出して止まる。
/// ディスクに書いていないものを書いてから、終了する(パニックではない、ふつうの終わり方)。
pub fn shutdown(code: u32) -> ! {
    if let Err(e) = user::sync_filesystems().and_then(|_| kernel::block::sync_all()) {
        kernel::println!("sync failed: {e:?}");
    }
    exit(code)
}

pub fn exit(code: u32) -> ! {
    if cfg!(feature = "semihosting") {
        arch_aarch64::semihosting::exit(code)
    }
    kernel::println!("system halted (status {code})");
    arch_aarch64::cpu::halt()
}

/// `<名前>-demo` feature 用:わざと異常を起こす。
pub fn run_demos(panic_demo: bool, fault_demo: bool) {
    if panic_demo {
        panic!("panic demo");
    }
    if fault_demo {
        // SAFETY: 未定義命令で例外を起こすだけ(未処理なのでパニックになる)
        unsafe { core::arch::asm!("udf #0") };
    }
}

/// `/etc/init.rc` に書かれたコマンドを順に実行する(フェーズ1の init)。
pub fn run_init() {
    let script = match user::read_file("/etc/init.rc") {
        Ok(bytes) => bytes,
        Err(e) => {
            kernel::println!("init: cannot read /etc/init.rc: {e:?}");
            return;
        }
    };
    let script = core::str::from_utf8(&script).unwrap_or("");
    for args in kernel::init::parse(script) {
        let argv: alloc::vec::Vec<&[u8]> = args.iter().map(|a| a.as_bytes()).collect();
        match run_program(args[0], &argv) {
            Ok(status) => kernel::println!("init: {} exited with status {status}", args[0]),
            Err(e) => kernel::println!("init: {} failed to start: {e:?}", args[0]),
        }
    }
}
