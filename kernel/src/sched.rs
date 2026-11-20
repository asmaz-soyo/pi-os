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
    pub regs: [u64; 16],
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
                state: State::Ready,
                ctx,
                stack: Some(bottom),
            }),
        );
        s.rq.push(tid);
        tid
    };
    (p.irq_restore)(flags);
    Some(tid)
}

/// 次のタスクに切り替える。`still_runnable` が false なら、今のタスクは列に戻さない。
fn schedule(new_state: State) {
    let p = platform();
    let flags = (p.irq_save)();
    let switch = {
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
        match s.rq.next(cur, runnable) {
            Some(next) => {
                let cur_task = s.tasks.get_mut(&cur).expect("current task");
                cur_task.state = if runnable { State::Ready } else { new_state };
                if new_state == State::Dead {
                    s.dead.push(cur);
                }
                let from: *mut SavedContext = &mut cur_task.ctx;
                let next_task = s.tasks.get_mut(&next).expect("queued task");
                next_task.state = State::Running;
                let to: *const SavedContext = &next_task.ctx;
                s.current = next;
                Some((from, to))
            }
            None => {
                assert!(runnable, "no task to run");
                None
            }
        }
    };
    if let Some((from, to)) = switch {
        // SAFETY: タスクは Box に入っているので、ロックを離しても文脈の場所は動かない
        unsafe { (p.switch)(from, to) };
    }
    (p.irq_restore)(flags);
}

/// ほかに動けるタスクがあれば譲る。
pub fn yield_now() {
    schedule(State::Running);
}

/// 今のタスクを終える。
pub fn exit_current() -> ! {
    schedule(State::Dead);
    unreachable!("a dead task was scheduled again");
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
}
