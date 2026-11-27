// signals:シグナルのハンドラ、マスク、別のスタック、既定の動作(Linux と同じ結果になるかを比べる)。
#define _GNU_SOURCE
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile int usr1, usr2, chld, nested, depth, max_depth, signo_ok, on_alt;
static char alt[64 * 1024];

static void on_usr1(int sig, siginfo_t *info, void *uc) {
    (void)uc;
    usr1++;
    signo_ok = sig == SIGUSR1 && info->si_signo == SIGUSR1;
    if (++depth > max_depth)
        max_depth = depth;
    // ハンドラの中では同じシグナルはマスクされている:送っても、戻ってから届く
    if (usr1 == 2) {
        raise(SIGUSR1);
        nested = usr1; // まだ 2 のはず
    }
    depth--;
}

static void on_usr2(int sig) {
    (void)sig;
    char here;
    usr2++;
    on_alt = &here >= alt && &here < alt + sizeof alt;
}

static void on_chld(int sig) {
    (void)sig;
    chld++;
}

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = on_usr1;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGUSR1, &sa, NULL);

    // 1. ハンドラ。戻ったあと、ローカルの値(レジスタ)はそのまま
    volatile long keep = 12345;
    raise(SIGUSR1);
    printf("handler: called %d, siginfo ok %d, registers kept %d\n", usr1, signo_ok, keep == 12345);

    // 2. ハンドラの中で同じシグナル:入れ子にならず、戻ってから届く
    raise(SIGUSR1);
    printf("no nesting: inside saw %d, after %d, max depth %d\n", nested, usr1, max_depth);

    // 3. マスク:保留になり、外すと届く
    sigset_t set, pend;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR1);
    sigprocmask(SIG_BLOCK, &set, NULL);
    raise(SIGUSR1);
    sigpending(&pend);
    int held = usr1 == 3 && sigismember(&pend, SIGUSR1);
    sigprocmask(SIG_UNBLOCK, &set, NULL);
    printf("mask: held while blocked %d, delivered after %d\n", held, usr1 == 4);

    // 4. 別のスタック(sigaltstack + SA_ONSTACK)、SA_RESETHAND(1 回で既定に戻る)
    stack_t ss = {.ss_sp = alt, .ss_size = sizeof alt, .ss_flags = 0};
    sigaltstack(&ss, NULL);
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = on_usr2;
    sa.sa_flags = SA_ONSTACK | SA_RESETHAND;
    sigaction(SIGUSR2, &sa, NULL);
    raise(SIGUSR2);
    struct sigaction now;
    sigaction(SIGUSR2, NULL, &now);
    printf("altstack: ran on it %d, reset to default %d\n", on_alt, now.sa_handler == SIG_DFL);

    // 5. 無視
    signal(SIGUSR1, SIG_IGN);
    raise(SIGUSR1);
    printf("ignored: still %d\n", usr1);

    // 6. 子が終わると SIGCHLD。子に SIGTERM を送ると、既定の動作で終わる
    signal(SIGCHLD, on_chld);
    pid_t c = fork();
    if (c == 0) {
        for (;;)
            pause();
    }
    kill(c, SIGTERM);
    int status;
    waitpid(c, &status, 0);
    printf("child: killed by signal %d, sigchld %d\n", WIFSIGNALED(status) ? WTERMSIG(status) : 0,
           chld >= 1);
    return 0;
}
