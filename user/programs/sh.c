// Pi-OS の小さなシェル。1 行読んで空白で区切り、組み込みコマンドか外部のプログラムを動かす。
//
// 組み込み:cd、pwd、exit
// 外部:名前に / がなければ /bin/<名前> を、あればそのパスを fork + execve で動かし、wait する。
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

#define MAX_ARGS 32

extern char **environ;

static int split(char *line, char **argv) {
    int argc = 0;
    for (char *tok = strtok(line, " \t\n"); tok && argc < MAX_ARGS - 1; tok = strtok(NULL, " \t\n")) {
        argv[argc++] = tok;
    }
    argv[argc] = NULL;
    return argc;
}

static int run(char **argv) {
    char path[256];
    if (strchr(argv[0], '/')) {
        snprintf(path, sizeof path, "%s", argv[0]);
    } else {
        snprintf(path, sizeof path, "/bin/%s", argv[0]);
    }
    pid_t pid = fork();
    if (pid < 0) {
        perror("fork");
        return 1;
    }
    if (pid == 0) {
        execve(path, argv, environ);
        fprintf(stderr, "sh: %s: %s\n", argv[0], strerror(errno));
        _exit(127);
    }
    int status;
    if (waitpid(pid, &status, 0) < 0) {
        perror("waitpid");
        return 1;
    }
    if (WIFSIGNALED(status)) return 128 + WTERMSIG(status);
    return WEXITSTATUS(status);
}

int main(void) {
    char line[512];
    char *argv[MAX_ARGS];
    int last = 0;
    for (;;) {
        char cwd[256];
        if (!getcwd(cwd, sizeof cwd)) strcpy(cwd, "?");
        printf("pi-os:%s$ ", cwd);
        fflush(stdout);
        if (!fgets(line, sizeof line, stdin)) {
            putchar('\n');
            return last;  // ^D
        }
        if (split(line, argv) == 0) continue;
        if (strcmp(argv[0], "exit") == 0) {
            return argv[1] ? atoi(argv[1]) : last;
        } else if (strcmp(argv[0], "cd") == 0) {
            last = chdir(argv[1] ? argv[1] : "/") == 0 ? 0 : 1;
            if (last) perror("cd");
        } else if (strcmp(argv[0], "pwd") == 0) {
            puts(cwd);
            last = 0;
        } else {
            last = run(argv);
            if (last) printf("[exit %d]\n", last);
        }
    }
}
