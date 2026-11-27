//! スケジューラ(タスクの切り替え)。
//!
//! タスクはそれぞれ自分のカーネルスタックと、保存した CPU の文脈(`SavedContext`)を持つ。
//! [`yield_now`] を呼ぶと、実行待ちの列(ラウンドロビン)から次のタスクを選び、文脈を切り替える。
//! 文脈の中身と切り替え方は CPU に依存するので、BSP が [`Platform`] として登録する。
//!
//! 今は協調的(タスクが自分から譲る)。タイマ割り込みでの切り替え(プリエンプション)は次のステップ。

use crate::sync::SpinLock;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;

pub type Tid = u32;

/// 実行待ちの列。
#[derive(Debug, Default)]
pub struct RunQueue {
    queue: VecDeque<Tid>,
}

impl RunQueue {
    pub const fn new() -> Self {
        Self {
            queue: VecDeque::new(),
        }
    }

    pub fn push(&mut self, tid: Tid) {
        if !self.queue.contains(&tid) {
            self.queue.push_back(tid);
        }
    }

    pub fn pop(&mut self) -> Option<Tid> {
        self.queue.pop_front()
    }

    pub fn remove(&mut self, tid: Tid) {
        self.queue.retain(|&t| t != tid);
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// 優先度を考えて次に動かすタスクを選ぶ。`prio` はタスクの優先度(大きいほど先)。
    /// 一番高い優先度のうち、先に並んだものを選ぶ。今のタスクがまだ動けて、
    /// それより優先度が高ければ `None`(今のタスクが動き続ける)。
    pub fn next_by(
        &mut self,
        current: Tid,
        current_runnable: bool,
        prio: impl Fn(Tid) -> u8,
    ) -> Option<Tid> {
        let best = self.queue.iter().copied().max_by(|a, b| {
            // max_by は同じなら後ろを選ぶので、比べ方を逆にして先頭を選ばせる
            prio(*a).cmp(&prio(*b)).then(core::cmp::Ordering::Greater)
        })?;
        if current_runnable && prio(current) > prio(best) {
            return None;
        }
        self.remove(best);
        if current_runnable {
            self.push(current);
        }
        Some(best)
    }

    /// 次に動かすタスクを選ぶ。今のタスク `current` がまだ動けるなら列の最後に回す。
    /// ほかに動けるタスクがなければ `None`(今のタスクが動き続ける、またはするべきことがない)。
    pub fn next(&mut self, current: Tid, current_runnable: bool) -> Option<Tid> {
        let next = self.pop()?;
        if current_runnable {
            self.push(current);
        }
        Some(next)
    }
}

/// 保存した CPU の文脈。中身は CPU 依存で、カーネルは触らない。
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct SavedContext {
    pub regs: [u64; 24],
}

/// 新しいスレッドの文脈を作る関数:`(スタックの一番上, entry, arg, exit)`
pub type NewContext = fn(usize, fn(usize), usize, fn()) -> SavedContext;

/// CPU 依存の操作。BSP が [`init`] で登録する。
#[derive(Clone, Copy)]
pub struct Platform {
    /// 今の文脈を `from` に保存し、`to` の文脈に切り替える
    pub switch: unsafe fn(from: *mut SavedContext, to: *const SavedContext),
    /// `stack_top` のスタックで `entry(arg)` から始まる文脈を作る。`entry` が戻ったら `exit` を呼ぶ
    pub new_context: NewContext,
    /// カーネルスタックを確保する(戻り値は (一番下, 一番上))
    pub alloc_stack: fn() -> Option<(usize, usize)>,
    pub free_stack: fn(bottom: usize),
    /// 割り込みを禁止し、前の状態を返す / 戻す
    pub irq_save: fn() -> bool,
    pub irq_restore: fn(bool),
    /// 次のタスクに切り替える直前に呼ばれる(ユーザー空間の切り替えなど)
    pub before_switch: fn(next: Tid),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Ready,
    Running,
    Blocked,
    Dead,
}

struct Task {
    state: State,
    ctx: SavedContext,
    /// カーネルスタックの一番下(起動時のタスクは持たない)
    stack: Option<usize>,
    /// 優先度(0 = 通常、1〜99 = リアルタイム)
    priority: u8,
    /// 眠っているなら、起きる時刻(起動からのナノ秒)
    wake_at: Option<u64>,
    /// 待ちのループで譲った(次に動くまで、優先度を 0 として扱う)
    yielded: bool,
    /// 眠っていないときに `wake` された(次の眠りをすぐ終わらせる。起こしたことを失わない)
    woken: bool,
}

struct Scheduler {
    tasks: BTreeMap<Tid, Box<Task>>,
    rq: RunQueue,
    current: Tid,
    next_tid: Tid,
    /// 終わったが、まだスタックを返していないタスク
    dead: Vec<Tid>,
}

static PLATFORM: spin::Once<Platform> = spin::Once::new();
static SCHED: SpinLock<Option<Scheduler>> = SpinLock::new(None);

fn platform() -> &'static Platform {
    PLATFORM.get().expect("scheduler is initialized")
}

/// スケジューラを始める。呼んだ文脈(起動処理)がタスク 0 になる。
pub fn init(p: Platform) {
    PLATFORM.call_once(|| p);
    let mut tasks = BTreeMap::new();
    tasks.insert(
        0,
        Box::new(Task {
            state: State::Running,
            ctx: SavedContext::default(),
            stack: None,
            priority: 0,
            wake_at: None,
            yielded: false,
            woken: false,
        }),
    );
    *SCHED.lock() = Some(Scheduler {
        tasks,
        rq: RunQueue::new(),
        current: 0,
        next_tid: 1,
        dead: Vec::new(),
    });
}

/// 今動いているタスクの番号。
pub fn current() -> Tid {
    SCHED.lock().as_ref().map_or(0, |s| s.current)
}

fn thread_exit() {
    exit_current()
}

/// カーネルスレッドを作り、実行待ちの列に入れる。
pub fn spawn(entry: fn(usize), arg: usize) -> Option<Tid> {
    spawn_in(entry, arg, true)
}

/// 止めたままタスクを作る。`wake(tid)` で動き出す。
/// 作ってから表(プロセスの表など)に入れるまでのあいだに、割り込みで切り替わって
/// 先に動き出さないようにするため
pub fn spawn_paused(entry: fn(usize), arg: usize) -> Option<Tid> {
    spawn_in(entry, arg, false)
}

fn spawn_in(entry: fn(usize), arg: usize, run: bool) -> Option<Tid> {
    let p = platform();
    let (bottom, top) = (p.alloc_stack)()?;
    let ctx = (p.new_context)(top, entry, arg, thread_exit);
    let flags = (p.irq_save)();
    let tid = {
        let mut guard = SCHED.lock();
        let s = guard.as_mut()?;
        let tid = s.next_tid;
        s.next_tid += 1;
        s.tasks.insert(
            tid,
            Box::new(Task {
                // 止めたままのタスクは、起きる時刻のない眠り(wake でだけ起きる)
                state: if run { State::Ready } else { State::Blocked },
                ctx,
                stack: Some(bottom),
                priority: 0,
                wake_at: None,
                yielded: false,
                woken: false,
            }),
        );
        if run {
            s.rq.push(tid);
        }
        tid
    };
    (p.irq_restore)(flags);
    Some(tid)
}

/// 起きる時刻を過ぎた眠りのタスクを、実行待ちの列に戻す。
/// まだ眠っているタスクのうち一番早い起床時刻に、タイマの割り込みを頼む
/// (ティックを待たずに起こせるので、遅れがティックの間隔に縛られない)。
fn wake_expired(s: &mut Scheduler) {
    let now = crate::time::now_ns();
    let mut earliest = None::<u64>;
    for (&tid, task) in s.tasks.iter_mut() {
        if task.state != State::Blocked {
            continue;
        }
        match task.wake_at {
            Some(w) if w <= now => {
                task.state = State::Ready;
                task.wake_at = None;
                s.rq.push(tid);
            }
            Some(w) => earliest = Some(earliest.map_or(w, |e| e.min(w))),
            None => {}
        }
    }
    if let Some(w) = earliest {
        crate::time::wake_at(w);
    }
}

/// 次のタスクの選び方
#[derive(Clone, Copy, PartialEq)]
enum Pick {
    /// 優先度の高いものから
    ByPriority,
    /// 優先度に関係なく、列の先頭(待ちの間に譲るとき。高い優先度のタスクが
    /// 待ちのループで CPU を独り占めすると、待っている相手が永遠に動けない)
    Anyone,
}

/// 次のタスクに切り替える。`new_state` が Running なら今のタスクはまだ動ける。
/// Blocked なら `wake_at` まで眠る。Dead なら終わる。
fn schedule(new_state: State, wake_at: Option<u64>, pick: Pick) {
    // ロックを持ったまま切り替えると、数(preempt_count)が次のタスクに持ち越され、
    // 同じロックを待つタスクが回り続ける
    debug_assert_eq!(
        crate::sync::preempt_count(),
        0,
        "switching tasks while holding a lock"
    );
    let p = platform();
    let flags = (p.irq_save)();
    let switch = loop {
        let mut guard = SCHED.lock();
        let s = guard.as_mut().expect("scheduler is initialized");
        // 前に終わったタスクのスタックを返す(自分のスタックの上では返せないので、ここで)
        for tid in core::mem::take(&mut s.dead) {
            if tid != s.current
                && let Some(task) = s.tasks.remove(&tid)
                && let Some(bottom) = task.stack
            {
                (p.free_stack)(bottom);
            }
        }
        let cur = s.current;
        let runnable = new_state == State::Running;
        // 待ちのループにいる間は優先度を手放す。そうしないと、高い優先度のタスクが
        // 待つたびに選ばれ続け、待っている相手(低い優先度)がほとんど動けない
        s.tasks.get_mut(&cur).expect("current task").yielded = pick == Pick::Anyone;
        if !runnable {
            let cur_task = s.tasks.get_mut(&cur).expect("current task");
            if cur_task.state == State::Running {
                cur_task.state = new_state;
                cur_task.wake_at = wake_at;
                // 眠る前に起こされていたら、すぐ起きる(下の wake_expired が列に戻す)
                if new_state == State::Blocked && core::mem::take(&mut cur_task.woken) {
                    cur_task.wake_at = Some(0);
                }
                if new_state == State::Dead {
                    s.dead.push(cur);
                }
            }
        }
        wake_expired(s);
        let tasks = &s.tasks;
        let next = match pick {
            Pick::ByPriority => s.rq.next_by(cur, runnable, |t| {
                tasks
                    .get(&t)
                    .map_or(0, |t| if t.yielded { 0 } else { t.priority })
            }),
            Pick::Anyone => s.rq.next(cur, runnable),
        };
        match next {
            None if runnable => break None,
            // 眠っていた自分がもう起きる時刻になった
            Some(next) if next == cur => {
                s.tasks.get_mut(&cur).expect("current task").state = State::Running;
                break None;
            }
            Some(next) => {
                let cur_task = s.tasks.get_mut(&cur).expect("current task");
                if runnable {
                    cur_task.state = State::Ready;
                }
                let from: *mut SavedContext = &mut cur_task.ctx;
                let next_task = s.tasks.get_mut(&next).expect("queued task");
                next_task.state = State::Running;
                next_task.yielded = false;
                next_task.yielded = false;
                let to: *const SavedContext = &next_task.ctx;
                s.current = next;
                break Some((from, to, next));
            }
            None => {
                // 動けるタスクがない:眠っているタスクが起きるまで待つ
                assert!(
                    s.tasks.values().any(|t| t.wake_at.is_some()),
                    "no task to run"
                );
                drop(guard);
                core::hint::spin_loop();
            }
        }
    };
    if let Some((from, to, next)) = switch {
        (p.before_switch)(next);
        // SAFETY: タスクは Box に入っているので、ロックを離しても文脈の場所は動かない
        unsafe { (p.switch)(from, to) };
    }
    (p.irq_restore)(flags);
}

/// 時刻 `deadline`(起動からのナノ秒)まで、今のタスクを眠らせる。
/// スケジューラが動いていなければ、その場で待つ。
pub fn sleep_until(deadline: u64) {
    if PLATFORM.get().is_none() || SCHED.lock().is_none() {
        while crate::time::now_ns() < deadline {
            core::hint::spin_loop();
        }
        return;
    }
    if crate::time::now_ns() < deadline {
        schedule(State::Blocked, Some(deadline), Pick::ByPriority);
    }
}

/// `deadline` まで必ず眠る(起こされても、期限の前なら眠り直す。nanosleep 用)
pub fn sleep_through(deadline: u64) {
    while crate::time::now_ns() < deadline {
        sleep_until(deadline);
    }
}

/// 眠っているタスク `tid` を、起きる時刻の前に起こす(割り込みのハンドラから呼んでよい)。
/// まだ眠っていなければ、次の眠りをすぐ終わらせる。ないタスクなら何もしない
pub fn wake(tid: Tid) {
    let Some(p) = PLATFORM.get() else {
        return;
    };
    let flags = (p.irq_save)();
    if let Some(s) = SCHED.lock().as_mut()
        && let Some(task) = s.tasks.get_mut(&tid)
    {
        match task.state {
            State::Blocked => {
                task.state = State::Ready;
                task.wake_at = None;
                s.rq.push(tid);
            }
            State::Dead => {}
            _ => task.woken = true,
        }
    }
    (p.irq_restore)(flags);
    request_resched();
}

/// タスクの優先度を変える(0 = 通常、1〜99 = リアルタイム)。タスクがなければ false。
pub fn set_priority(tid: Tid, priority: u8) -> bool {
    SCHED
        .lock()
        .as_mut()
        .and_then(|s| s.tasks.get_mut(&tid))
        .map(|t| t.priority = priority)
        .is_some()
}

pub fn priority(tid: Tid) -> u8 {
    SCHED
        .lock()
        .as_ref()
        .and_then(|s| s.tasks.get(&tid))
        .map_or(0, |t| t.priority)
}

/// ほかに動けるタスクがあれば譲る。
pub fn yield_now() {
    schedule(State::Running, None, Pick::ByPriority);
}

/// スケジューラが動いているか(動いていなければ、待つときは回って待つ)
pub fn running() -> bool {
    PLATFORM.get().is_some() && SCHED.lock().is_some()
}

/// タスクが眠っている(Blocked)か。/proc/<pid>/status の State に使う
pub fn is_sleeping(tid: Tid) -> bool {
    SCHED
        .lock()
        .as_ref()
        .and_then(|s| s.tasks.get(&tid))
        .is_some_and(|t| t.state == State::Blocked)
}

/// 何かを待っている間に呼ぶ:スケジューラが動いていれば譲り、なければその場で少し待つ。
pub fn relax() {
    if running() {
        schedule(State::Running, None, Pick::Anyone);
    } else {
        core::hint::spin_loop();
    }
}

/// 今のタスクを終える。
pub fn exit_current() -> ! {
    schedule(State::Dead, None, Pick::ByPriority);
    unreachable!("a dead task was scheduled again");
}

static RESCHED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// 切り替えが必要という印を付ける(タイマ割り込みから)。
pub fn request_resched() {
    RESCHED.store(true, core::sync::atomic::Ordering::Relaxed);
}

/// 印を見て消す。
pub fn take_resched() -> bool {
    RESCHED.swap(false, core::sync::atomic::Ordering::Relaxed)
}

/// タスクが生きているか(終わっていないか)。
pub fn is_alive(tid: Tid) -> bool {
    SCHED
        .lock()
        .as_ref()
        .is_some_and(|s| s.tasks.get(&tid).is_some_and(|t| t.state != State::Dead))
}

/// 動いているタスクの数(テストとデバッグ用)。
pub fn task_count() -> usize {
    SCHED.lock().as_ref().map_or(0, |s| {
        s.tasks.values().filter(|t| t.state != State::Dead).count()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_queue_is_round_robin() {
        let mut rq = RunQueue::new();
        rq.push(1);
        rq.push(2);
        rq.push(3);
        assert_eq!(rq.pop(), Some(1));
        rq.push(1);
        assert_eq!(rq.pop(), Some(2));
        assert_eq!(rq.pop(), Some(3));
        assert_eq!(rq.pop(), Some(1));
        assert_eq!(rq.pop(), None);
    }

    #[test]
    fn a_task_is_queued_only_once() {
        let mut rq = RunQueue::new();
        rq.push(7);
        rq.push(7);
        assert_eq!(rq.len(), 1);
    }

    #[test]
    fn removed_tasks_are_not_picked() {
        let mut rq = RunQueue::new();
        rq.push(1);
        rq.push(2);
        rq.remove(1);
        assert_eq!(rq.pop(), Some(2));
        assert_eq!(rq.pop(), None);
    }

    #[test]
    fn next_skips_the_current_task_when_others_are_ready() {
        let mut rq = RunQueue::new();
        rq.push(2);
        // 今のタスク(1)がまだ動けるなら、列の最後に回して次を選ぶ
        assert_eq!(rq.next(1, true), Some(2));
        assert_eq!(rq.pop(), Some(1));
    }

    #[test]
    fn next_keeps_the_current_task_when_nothing_else_is_ready() {
        let mut rq = RunQueue::new();
        assert_eq!(rq.next(1, true), None, "keep running");
        assert_eq!(rq.next(1, false), None, "nothing to run");
    }

    fn prio(p: &[(Tid, u8)]) -> impl Fn(Tid) -> u8 + '_ {
        move |t| p.iter().find(|(x, _)| *x == t).map_or(0, |(_, pr)| *pr)
    }

    #[test]
    fn the_highest_priority_ready_task_runs_first() {
        let mut rq = RunQueue::new();
        rq.push(2);
        rq.push(3);
        rq.push(4);
        let p = [(3, 50)];
        assert_eq!(rq.next_by(1, true, prio(&p)), Some(3));
        // 同じ優先度どうしは並んだ順(1 は列の最後に回った)
        assert_eq!(rq.next_by(3, false, prio(&p)), Some(2));
    }

    #[test]
    fn a_higher_priority_current_task_keeps_running() {
        let mut rq = RunQueue::new();
        rq.push(2);
        let p = [(1, 10)];
        assert_eq!(rq.next_by(1, true, prio(&p)), None, "1 outranks 2");
        assert_eq!(rq.len(), 1);
        // 動けなくなったら、低くても次を選ぶ
        assert_eq!(rq.next_by(1, false, prio(&p)), Some(2));
    }
}
