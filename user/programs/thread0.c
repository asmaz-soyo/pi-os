// thread0:clone で直接スレッドを作る(futex を使わない、いちばん小さいスレッドの確かめ)。
//
// スレッドは親と同じメモリ・同じ fd の表を使い、getpid は同じ番号、gettid は違う番号を返す。
// スレッドが終わると、カーネルが CLONE_CHILD_CLEARTID の場所に 0 を書く。親はそれを待つ。
//
// musl の公開の clone() は CLONE_THREAD と CLONE_CHILD_CLEARTID を断る(TLS が食い違うので。
// pthread_create は内部の __clone を使う)。ここでは clone のシステムコールを直接呼ぶ。
#define _GNU_SOURCE
#include <sched.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

static volatile int shared;
static volatile pid_t child_pid, child_tid;
static volatile pid_t ctid = -1;
static pid_t ptid;
static char stack[64 * 1024] __attribute__((aligned(16)));

// raw_clone(flags, stack, ptid, tls, ctid, fn, arg):x0〜x6。
// 新しいスタックに fn と arg を積んでから clone(220)を呼ぶ。子は新しいスタックで戻ってくるので、
// 積んだものを取り出して fn(arg) を呼び、exit(93)する(このスレッドだけが終わる)
long raw_clone(unsigned long flags, void *stack, pid_t *ptid, unsigned long tls, volatile pid_t *ctid,
               int (*fn)(void *), void *arg);
__asm__(".global raw_clone\n"
        "raw_clone:\n"
        "    stp x5, x6, [x1, #-16]!\n" // 子のスタックに (fn, arg)
        "    mov x8, #220\n"            // clone
        "    svc #0\n"
        "    cbz x0, 1f\n" // 親:子の番号(か -errno)を返す
        "    ret\n"
        "1:  ldp x1, x0, [sp], #16\n" // 子:x1 = fn、x0 = arg
        "    blr x1\n"
        "    mov x8, #93\n" // exit
        "    svc #0\n");

static int child(void *arg) {
    child_pid = getpid();
    child_tid = syscall(SYS_gettid);
    shared = *(int *)arg + 1; // 同じメモリに書く
    return 0;
}

int main(void) {
    int arg = 41;
    int flags = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD | CLONE_SYSVSEM |
                CLONE_PARENT_SETTID | CLONE_CHILD_CLEARTID;
    long tid = raw_clone(flags, stack + sizeof stack, &ptid, 0, &ctid, child, &arg);
    if (tid < 0) {
        fprintf(stderr, "thread0: clone: %ld\n", tid);
        return 1;
    }
    // スレッドが終わるまで待つ(カーネルが ctid に 0 を書く)
    while (ctid != 0)
        sched_yield();
    int ok = shared == 42 && child_pid == getpid() && child_tid == tid && tid != getpid() &&
             ptid == tid;
    printf("thread0: shared=%d same_pid=%d own_tid=%d ptid=%d -> %s\n", shared,
           child_pid == getpid(), child_tid == tid && tid != getpid(), ptid == tid,
           ok ? "ok" : "FAILED");
    return ok ? 0 : 1;
}
