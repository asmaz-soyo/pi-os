// sigwait:シグナルで待ちが中断される(EINTR)、SA_RESTART でやり直す、sigsuspend、SIGPIPE。
// 別のスレッドから少し後にシグナルを送り、待っているところに割り込ませる。
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static pthread_t main_thread;
static volatile int got;
static int fds[2];

static void handler(int sig) {
    (void)sig;
    got++;
}

static void nap_ms(long ms) {
    struct timespec t = {0, ms * 1000000};
    nanosleep(&t, NULL);
}

// 50ms 後に main に SIGUSR1。`then_write` なら、さらに 50ms 後にパイプに書く
static void *poke(void *then_write) {
    nap_ms(50);
    pthread_kill(main_thread, SIGUSR1);
    if (then_write) {
        nap_ms(50);
        write(fds[1], "x", 1);
    }
    return NULL;
}

static void set_handler(int sig, int flags) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = handler;
    sa.sa_flags = flags;
    sigaction(sig, &sa, NULL);
}

int main(void) {
    main_thread = pthread_self();
    pipe(fds);
    pthread_t t;
    char c;

    // 1. SA_RESTART なし:空のパイプの read が EINTR で戻る
    set_handler(SIGUSR1, 0);
    pthread_create(&t, NULL, poke, NULL);
    ssize_t n = read(fds[0], &c, 1);
    printf("interrupted read: ret %zd, EINTR %d, handler ran %d\n", n, n < 0 && errno == EINTR, got);
    pthread_join(t, NULL);

    // 2. SA_RESTART:ハンドラのあと read をやり直し、あとから書かれたものを読む
    set_handler(SIGUSR1, SA_RESTART);
    pthread_create(&t, NULL, poke, (void *)1);
    n = read(fds[0], &c, 1);
    printf("restarted read: ret %zd, byte %c, handler ran %d\n", n, n == 1 ? c : '?', got);
    pthread_join(t, NULL);

    // 3. sigsuspend:SIGUSR1 をマスクしたまま、空のマスクで待つ。戻るとマスクは元どおり
    sigset_t block, empty, now;
    sigemptyset(&block);
    sigaddset(&block, SIGUSR1);
    sigemptyset(&empty);
    sigprocmask(SIG_BLOCK, &block, NULL);
    pthread_create(&t, NULL, poke, NULL);
    int r = sigsuspend(&empty);
    sigprocmask(SIG_BLOCK, NULL, &now);
    printf("sigsuspend: ret %d, EINTR %d, handler ran %d, mask restored %d\n", r, errno == EINTR, got,
           sigismember(&now, SIGUSR1));
    pthread_join(t, NULL);
    sigprocmask(SIG_UNBLOCK, &block, NULL);

    // 4. 読む人のいないパイプに書く:EPIPE と SIGPIPE
    set_handler(SIGPIPE, 0);
    close(fds[0]);
    n = write(fds[1], "y", 1);
    printf("broken pipe: ret %zd, EPIPE %d, sigpipe handled %d\n", n, n < 0 && errno == EPIPE, got);
    return 0;
}
