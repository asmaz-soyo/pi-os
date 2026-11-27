// tcpbulk <ポート> <バイト数> [プロトコル]:ループバックの TCP で大きなデータを流し、時間を測る。
//
// fork して、子がサーバー(listen → accept → EOF まで読む)、親がクライアント(connect → 書く →
// shutdown(SHUT_WR) → 相手が閉じるまで待つ)になる。中身は i % 251 の並びで、子は数と中身を確かめて
// 終了ステータス(0 = 全部正しく届いた)で答える。
// プロトコルは socket の 3 つ目の引数(0 = 既定、253 = Pi-OS の自作 TCP)。
#include <arpa/inet.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static long now_us(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec * 1000000L + t.tv_nsec / 1000;
}

static int server(int port, long bytes, int proto, int ready) {
    int s = socket(AF_INET, SOCK_STREAM, proto);
    struct sockaddr_in a = {.sin_family = AF_INET, .sin_port = htons(port)};
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    if (s < 0 || bind(s, (struct sockaddr *)&a, sizeof a) < 0 || listen(s, 1) < 0) {
        perror("tcpbulk: server");
        return 3;
    }
    write(ready, "!", 1); // 待ち受けを始めた
    int c = accept(s, NULL, NULL);
    if (c < 0) {
        perror("tcpbulk: accept");
        return 3;
    }
    static unsigned char buf[8192];
    long got = 0;
    int bad = 0;
    for (;;) {
        ssize_t n = read(c, buf, sizeof buf);
        if (n < 0) {
            perror("tcpbulk: read");
            return 4;
        }
        if (n == 0)
            break;
        for (ssize_t i = 0; i < n; i++)
            bad |= buf[i] != (unsigned char)((got + i) % 251);
        got += n;
    }
    close(c);
    if (got != bytes || bad) {
        fprintf(stderr, "tcpbulk: got %ld of %ld bytes%s\n", got, bytes, bad ? ", corrupted" : "");
        return 1;
    }
    return 0;
}

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: tcpbulk PORT BYTES [PROTOCOL]\n");
        return 2;
    }
    int port = atoi(argv[1]);
    long bytes = atol(argv[2]);
    int proto = argc > 3 ? atoi(argv[3]) : 0;
    int p[2];
    pipe(p);
    pid_t pid = fork();
    if (pid == 0) {
        close(p[0]);
        _exit(server(port, bytes, proto, p[1]));
    }
    close(p[1]);
    char ok;
    read(p[0], &ok, 1); // 子が待ち受けを始めるまで待つ

    long start = now_us();
    int s = socket(AF_INET, SOCK_STREAM, proto);
    struct sockaddr_in a = {.sin_family = AF_INET, .sin_port = htons(port)};
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    if (s < 0 || connect(s, (struct sockaddr *)&a, sizeof a) < 0) {
        perror("tcpbulk: connect");
        return 3;
    }
    static unsigned char buf[8192];
    long sent = 0;
    while (sent < bytes) {
        long n = bytes - sent < (long)sizeof buf ? bytes - sent : (long)sizeof buf;
        for (long i = 0; i < n; i++)
            buf[i] = (unsigned char)((sent + i) % 251);
        ssize_t w = write(s, buf, n);
        if (w <= 0) {
            perror("tcpbulk: write");
            return 4;
        }
        sent += w;
    }
    shutdown(s, SHUT_WR); // これで全部(相手は EOF を読む)
    while (read(s, buf, sizeof buf) > 0) {
    } // 相手が閉じるまで
    close(s);
    int status;
    waitpid(pid, &status, 0);
    long us = now_us() - start;
    int code = WIFEXITED(status) ? WEXITSTATUS(status) : 5;
    printf("tcpbulk: %ld bytes in %ld ms (%ld KB/s)%s\n", bytes, us / 1000,
           us ? bytes * 1000 / us : 0, code ? " FAILED" : "");
    return code;
}
