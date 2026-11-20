//! AArch64 ボード共通の起動処理(初期化の順番)と、BSP が埋める `Board` トレイト。
//!
//! BSP は「どのドライバをどのアドレスで使うか」だけを [`Board`] として書き、
//! 初期化の順番やパニックの処理はここで共通にする。
#![no_std]

extern crate alloc;

mod user;

pub use user::{STATUS_SEGV, run_program, run_user};
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
    // 5. タイマ割り込み
    start_timer::<B>();
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
        kernel::process::before_return_to_user();
    }

    fn irq(&self, _frame: &mut TrapFrame) {
        kernel::irq::IRQS.dispatch(&mut *B::intc().lock());
    }
}

fn timer_tick() {
    kernel::time::CLOCK.tick(&mut GenericTimer);
}

fn start_timer<B: Board>() {
    let handler: &'static Exceptions<B> = &Exceptions(PhantomData);
    exception::set_handler(Some(handler));
    kernel::irq::IRQS
        .register(B::TIMER_IRQ, timer_tick)
        .expect("timer irq number is valid");
    B::intc().lock().enable(B::TIMER_IRQ);
    kernel::time::CLOCK.start(&mut GenericTimer, B::TIMER_HZ);
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

/// 終了する。QEMU ならセミホスティングで終了コードを返す。
pub fn exit(code: u32) -> ! {
    arch_aarch64::semihosting::exit(code)
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
    let script = core::str::from_utf8(script).unwrap_or("");
    for args in kernel::init::parse(script) {
        let argv: alloc::vec::Vec<&[u8]> = args.iter().map(|a| a.as_bytes()).collect();
        match run_program(args[0], &argv) {
            Ok(status) => kernel::println!("init: {} exited with status {status}", args[0]),
            Err(e) => kernel::println!("init: {} failed to start: {e:?}", args[0]),
        }
    }
}
