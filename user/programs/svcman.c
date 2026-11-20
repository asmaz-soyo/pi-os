// 小さなサービスマネージャ。設定ファイルのサービスを、起動のきっかけ(トリガー)に応じて動かす。
//
//   [service.名前]
//   exec = "/bin/プログラム 引数"
//   trigger = "startup"            起動時に 1 回
//   trigger = { every = "50ms" }   一定間隔で(timerfd + epoll)。単位は ms か s
//   restart = "on-failure"         0 以外で終わったら起動し直す
//   max_runs = 3                   動かす回数の上限(周期と再起動の両方に効く)
//
// すべてのサービスが上限に達するか終わったら、動かした回数の合計を終了ステータスにして終わる。
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/timerfd.h>
#include <sys/wait.h>
#include <unistd.h>

#define MAX_SERVICES 16
#define MAX_ARGS 8

struct service {
    char name[32];
    char exec[128];
    long every_ms;      // 0 なら startup
    int restart;        // on-failure
    int max_runs;       // 0 なら無制限
    int runs;
    int timer_fd;
    int done;
};

static struct service services[MAX_SERVICES];
static int count;

static char *trim(char *s) {
    while (*s == ' ' || *s == '\t') s++;
    char *e = s + strlen(s);
    while (e > s && (e[-1] == ' ' || e[-1] == '\t' || e[-1] == '\n' || e[-1] == '\r')) *--e = 0;
    return s;
}

/// "..." の中身を取り出す
static int quoted(const char *s, char *out, size_t size) {
    const char *a = strchr(s, '"');
    const char *b = a ? strchr(a + 1, '"') : NULL;
    if (!a || !b || (size_t)(b - a - 1) >= size) return -1;
    memcpy(out, a + 1, b - a - 1);
    out[b - a - 1] = 0;
    return 0;
}

static long duration_ms(const char *s) {
    char *end;
    long v = strtol(s, &end, 10);
    if (!strcmp(end, "ms")) return v;
    if (!strcmp(end, "s")) return v * 1000;
    return -1;
}

static int load(const char *path) {
    FILE *f = fopen(path, "r");
    if (!f) {
        perror(path);
        return -1;
    }
    char line[256];
    struct service *cur = NULL;
    while (fgets(line, sizeof line, f)) {
        char *l = trim(line);
        if (!*l || *l == '#') continue;
        if (!strncmp(l, "[service.", 9)) {
            if (count == MAX_SERVICES) return -1;
            cur = &services[count++];
            memset(cur, 0, sizeof *cur);
            cur->timer_fd = -1;
            char *end = strchr(l, ']');
            if (!end) return -1;
            *end = 0;
            snprintf(cur->name, sizeof cur->name, "%s", l + 9);
            continue;
        }
        if (!cur) return -1;
        char *eq = strchr(l, '=');
        if (!eq) return -1;
        *eq = 0;
        char *key = trim(l), *val = trim(eq + 1);
        char text[128];
        if (!strcmp(key, "exec")) {
            if (quoted(val, cur->exec, sizeof cur->exec)) return -1;
        } else if (!strcmp(key, "trigger")) {
            if (strstr(val, "every")) {
                if (quoted(strstr(val, "every"), text, sizeof text)) return -1;
                if ((cur->every_ms = duration_ms(text)) <= 0) return -1;
            } else if (quoted(val, text, sizeof text) || strcmp(text, "startup")) {
                return -1;
            }
        } else if (!strcmp(key, "restart")) {
            if (quoted(val, text, sizeof text)) return -1;
            cur->restart = !strcmp(text, "on-failure");
        } else if (!strcmp(key, "max_runs")) {
            cur->max_runs = atoi(val);
        } else {
            return -1;
        }
    }
    fclose(f);
    return 0;
}

/// サービスを 1 回動かし、終わるのを待って終了ステータスを返す
static int run_once(struct service *s) {
    char cmd[128];
    snprintf(cmd, sizeof cmd, "%s", s->exec);
    char *argv[MAX_ARGS + 1];
    int argc = 0;
    for (char *t = strtok(cmd, " "); t && argc < MAX_ARGS; t = strtok(NULL, " ")) argv[argc++] = t;
    argv[argc] = NULL;
    s->runs++;
    pid_t pid = fork();
    if (pid == 0) {
        execv(argv[0], argv);
        _exit(127);
    }
    int status;
    waitpid(pid, &status, 0);
    int code = WIFEXITED(status) ? WEXITSTATUS(status) : 128 + WTERMSIG(status);
    printf("svcman: %s run %d exited with %d\n", s->name, s->runs, code);
    return code;
}

static int limit_reached(const struct service *s) {
    return s->max_runs && s->runs >= s->max_runs;
}

int main(int argc, char **argv) {
    const char *conf = argc > 1 ? argv[1] : "/etc/services.conf";
    if (load(conf)) {
        fprintf(stderr, "svcman: bad config %s\n", conf);
        return 255;
    }
    int ep = epoll_create1(0);
    int active = 0;
    // startup のサービスを動かし、周期のサービスのタイマを仕掛ける
    for (int i = 0; i < count; i++) {
        struct service *s = &services[i];
        if (s->every_ms == 0) {
            while (run_once(s) != 0 && s->restart && !limit_reached(s)) {}
            s->done = 1;
            continue;
        }
        s->timer_fd = timerfd_create(CLOCK_MONOTONIC, 0);
        struct itimerspec spec = {
            .it_interval = {s->every_ms / 1000, (s->every_ms % 1000) * 1000000},
            .it_value = {s->every_ms / 1000, (s->every_ms % 1000) * 1000000},
        };
        timerfd_settime(s->timer_fd, 0, &spec, NULL);
        struct epoll_event ev = {.events = EPOLLIN, .data.u32 = i};
        epoll_ctl(ep, EPOLL_CTL_ADD, s->timer_fd, &ev);
        active++;
    }
    // 周期のサービス:タイマが満了するたびに動かす
    while (active > 0) {
        struct epoll_event ev;
        if (epoll_wait(ep, &ev, 1, -1) != 1) break;
        struct service *s = &services[ev.data.u32];
        uint64_t expirations;
        read(s->timer_fd, &expirations, sizeof expirations);
        if (s->done) continue;
        run_once(s);
        if (limit_reached(s)) {
            s->done = 1;
            epoll_ctl(ep, EPOLL_CTL_DEL, s->timer_fd, NULL);
            active--;
        }
    }
    int total = 0;
    for (int i = 0; i < count; i++) total += services[i].runs;
    return total;
}
