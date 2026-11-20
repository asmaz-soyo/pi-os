// fork / execve / waitpid とシバンのテスト。すべてうまくいけば終了ステータス 0
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

static int run(const char *path, int want) {
    pid_t pid = fork();
    if (pid < 0) return 10;
    if (pid == 0) {
        execl(path, path, (char *)0);
        _exit(99);  // exec に失敗した
    }
    int status;
    if (waitpid(pid, &status, 0) != pid) return 11;
    if (!WIFEXITED(status) || WEXITSTATUS(status) != want) return 12;
    return 0;
}

int main(void) {
    int r;
    if ((r = run("/bin/exit42", 42))) return r;
    if ((r = run("/bin/greet.js", 0))) return r + 20;
    printf("fork ok, parent %d\n", getppid());
    return 0;
}
