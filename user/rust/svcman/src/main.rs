//! サービスマネージャ(Rust 製)。設定ファイルのサービスを、きっかけ(トリガー)に応じて動かす。
//!
//! - startup:起動時に 1 回(restart = "on-failure" なら、0 で終わるか上限まで動かし直す)
//! - every:timerfd を epoll で待ち、満了のたびに動かす
//! - gpio:`/dev/gpio/N` を epoll で待ち、割り込みのたびに動かす
//!
//! すべてのサービスが上限に達するか終わったら、動かした回数の合計を終了ステータスにして終わる。
//! epoll と timerfd は Rust の標準ライブラリにないので、libc(musl)の関数を直接呼ぶ(依存クレートなし)。

use std::ffi::CString;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use svcman::{Service, Trigger};

mod sys {
    use std::ffi::{c_char, c_int, c_long, c_void};

    pub const EPOLLIN: u32 = 1;
    pub const EPOLL_CTL_ADD: c_int = 1;
    pub const EPOLL_CTL_DEL: c_int = 2;
    pub const CLOCK_MONOTONIC: c_int = 1;
    pub const O_RDONLY: c_int = 0;

    /// AArch64 の `struct epoll_event` は詰めずに 16 バイト(x86_64 だけ packed)
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct EpollEvent {
        pub events: u32,
        pub data: u64,
    }

    #[repr(C)]
    pub struct Timespec {
        pub sec: c_long,
        pub nsec: c_long,
    }

    #[repr(C)]
    pub struct Itimerspec {
        pub interval: Timespec,
        pub value: Timespec,
    }

    unsafe extern "C" {
        pub fn epoll_create1(flags: c_int) -> c_int;
        pub fn epoll_ctl(ep: c_int, op: c_int, fd: c_int, ev: *mut EpollEvent) -> c_int;
        pub fn epoll_wait(ep: c_int, evs: *mut EpollEvent, max: c_int, timeout: c_int) -> c_int;
        pub fn timerfd_create(clock: c_int, flags: c_int) -> c_int;
        pub fn timerfd_settime(
            fd: c_int,
            flags: c_int,
            new: *const Itimerspec,
            old: *mut Itimerspec,
        ) -> c_int;
        pub fn open(path: *const c_char, flags: c_int, ...) -> c_int;
        pub fn read(fd: c_int, buf: *mut c_void, len: usize) -> isize;
    }
}

struct Running {
    service: Service,
    runs: u32,
    /// epoll で待つ fd(timerfd か /dev/gpio/N)
    fd: i32,
    done: bool,
}

impl Running {
    fn limit_reached(&self) -> bool {
        self.service.max_runs != 0 && self.runs >= self.service.max_runs
    }

    /// 1 回動かし、終わるのを待って終了ステータスを返す
    fn run_once(&mut self) -> i32 {
        self.runs += 1;
        let (prog, args) = self.service.exec.split_first().expect("exec is not empty");
        let code = match Command::new(prog).args(args).status() {
            Ok(st) => st.code().unwrap_or_else(|| 128 + st.signal().unwrap_or(0)),
            Err(_) => 127,
        };
        println!(
            "svcman: {} run {} exited with {code}",
            self.service.name, self.runs
        );
        code
    }
}

fn add(ep: i32, fd: i32, index: usize) {
    let mut ev = sys::EpollEvent {
        events: sys::EPOLLIN,
        data: index as u64,
    };
    // SAFETY: ev は呼び出しの間だけ使われる
    unsafe { sys::epoll_ctl(ep, sys::EPOLL_CTL_ADD, fd, &mut ev) };
}

fn timer(ms: u64) -> i32 {
    let ts = || sys::Timespec {
        sec: (ms / 1000) as _,
        nsec: ((ms % 1000) * 1_000_000) as _,
    };
    let spec = sys::Itimerspec {
        interval: ts(),
        value: ts(),
    };
    // SAFETY: 引数はこの関数の中で生きている
    unsafe {
        let fd = sys::timerfd_create(sys::CLOCK_MONOTONIC, 0);
        sys::timerfd_settime(fd, 0, &spec, std::ptr::null_mut());
        fd
    }
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/etc/services.conf".to_string());
    let services = match std::fs::read_to_string(&path)
        .map_err(|e| e.to_string())
        .and_then(|t| svcman::parse(&t))
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("svcman: bad config {path}: {e}");
            std::process::exit(255);
        }
    };
    let mut all: Vec<Running> = services
        .into_iter()
        .map(|service| Running {
            service,
            runs: 0,
            fd: -1,
            done: false,
        })
        .collect();

    // SAFETY: 引数のないシステムコール
    let ep = unsafe { sys::epoll_create1(0) };
    let mut active = 0;
    for (i, r) in all.iter_mut().enumerate() {
        match r.service.trigger {
            Trigger::Startup => {
                while r.run_once() != 0 && r.service.restart_on_failure && !r.limit_reached() {}
                r.done = true;
            }
            Trigger::Every(ms) => {
                r.fd = timer(ms);
                add(ep, r.fd, i);
                active += 1;
            }
            Trigger::Gpio(n) => {
                let p = CString::new(format!("/dev/gpio/{n}")).unwrap();
                // SAFETY: p は NUL で終わる文字列
                r.fd = unsafe { sys::open(p.as_ptr(), sys::O_RDONLY) };
                if r.fd < 0 {
                    eprintln!("svcman: cannot open /dev/gpio/{n}");
                    r.done = true;
                    continue;
                }
                add(ep, r.fd, i);
                active += 1;
            }
        }
    }
    if active > 0 {
        println!("svcman: watching {active} service(s)");
        let _ = std::io::stdout().flush();
    }
    while active > 0 {
        let mut ev = sys::EpollEvent::default();
        // SAFETY: ev に 1 つ書かれる
        if unsafe { sys::epoll_wait(ep, &mut ev, 1, -1) } != 1 {
            break;
        }
        let r = &mut all[ev.data as usize];
        // 満了回数(timerfd)か割り込みの回数(gpio)を読んで消す
        let mut count = 0u64;
        // SAFETY: count は 8 バイト
        unsafe { sys::read(r.fd, (&mut count as *mut u64).cast(), 8) };
        if r.done {
            continue;
        }
        r.run_once();
        if r.limit_reached() {
            r.done = true;
            // SAFETY: 登録した fd を外すだけ
            unsafe { sys::epoll_ctl(ep, sys::EPOLL_CTL_DEL, r.fd, std::ptr::null_mut()) };
            active -= 1;
        }
    }
    let total: u32 = all.iter().map(|r| r.runs).sum();
    std::process::exit(total as i32);
}
