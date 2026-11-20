// リアルタイム優先度のジッタ計測(cyclictest の小さな版)。
// 低い優先度で CPU を回し続ける子を作り、自分は SCHED_FIFO で 5ms ごとに起きる。
// 予定の時刻から実際に起きた時刻までの遅れ(レイテンシ)を測り、最大値が小さいことを確かめる。
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define PERIOD_NS 5000000ULL
#define CYCLES 100

static uint64_t now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ULL + ts.tv_nsec;
}

int main(int argc, char **argv) {
    // 引数 "normal" なら優先度を上げずに測る(比べるため)
    int realtime = !(argc > 1 && argv[1][0] == 'n');
    uint64_t limit_us = argc > 2 ? strtoull(argv[2], 0, 10) : 0;
    uint64_t end = now_ns() + (CYCLES + 20) * PERIOD_NS;

    pid_t spinner = fork();
    if (spinner < 0) return 1;
    if (spinner == 0) {
        // 子:計測が終わるまで CPU を回し続ける
        while (now_ns() < end) {
        }
        return 0;
    }
    if (realtime) {
        struct sched_param sp = {.sched_priority = 80};
        // musl の sched_setscheduler は ENOSYS を返すだけなので、システムコールを直接呼ぶ
        if (syscall(SYS_sched_setscheduler, 0, SCHED_FIFO, &sp) != 0) return 2;
        if (syscall(SYS_sched_getscheduler, 0) != SCHED_FIFO) return 3;
    }

    uint64_t min = UINT64_MAX, max = 0, sum = 0;
    uint64_t next = now_ns() + PERIOD_NS;
    for (int i = 0; i < CYCLES; i++) {
        struct timespec ts = {next / 1000000000ULL, next % 1000000000ULL};
        clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &ts, NULL);
        uint64_t lat = now_ns() - next;
        if (lat < min) min = lat;
        if (lat > max) max = lat;
        sum += lat;
        next += PERIOD_NS;
    }
    int status;
    if (waitpid(spinner, &status, 0) != spinner) return 4;
    printf("jitter (%s): min %llu us, avg %llu us, max %llu us\n", realtime ? "SCHED_FIFO" : "normal",
           (unsigned long long)(min / 1000), (unsigned long long)(sum / CYCLES / 1000),
           (unsigned long long)(max / 1000));
    if (limit_us && max / 1000 > limit_us) return 5;
    return 0;
}
