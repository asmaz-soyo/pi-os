// timerfd + epoll で、20ms ごとに 3 回起きる(計画書の「周期」トリガーの形)。
#include <stdint.h>
#include <stdio.h>
#include <sys/epoll.h>
#include <sys/timerfd.h>
#include <time.h>
#include <unistd.h>

static uint64_t now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

int main(void) {
    int tfd = timerfd_create(CLOCK_MONOTONIC, 0);
    struct itimerspec spec = {.it_interval = {0, 20000000}, .it_value = {0, 20000000}};
    if (tfd < 0 || timerfd_settime(tfd, 0, &spec, NULL) != 0) return 1;
    int ep = epoll_create1(0);
    struct epoll_event ev = {.events = EPOLLIN, .data.u64 = 42};
    if (ep < 0 || epoll_ctl(ep, EPOLL_CTL_ADD, tfd, &ev) != 0) return 2;

    uint64_t start = now_ms();
    for (int i = 1; i <= 3; i++) {
        struct epoll_event got;
        if (epoll_wait(ep, &got, 1, -1) != 1 || got.data.u64 != 42) return 3;
        uint64_t n;
        if (read(tfd, &n, sizeof n) != sizeof n || n < 1) return 4;
        printf("tick %d\n", i);
    }
    uint64_t elapsed = now_ms() - start;
    if (elapsed < 55) return 5;  // 3 回 × 20ms(少しの誤差は許す)

    struct timespec nap = {0, 10000000};
    if (nanosleep(&nap, NULL) != 0) return 6;
    return 0;
}
