// システムコールの互換テスト(cargo xtask test --compat)。
// Linux(qemu-aarch64)と Pi-OS の両方で動かし、出力が同じになることを確かめる。
// 環境で変わるもの(uid、時刻、パス、プロセス番号)は出さない。
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static const char *errname(int e) {
    switch (e) {
    case EPERM: return "EPERM";
    case ENOENT: return "ENOENT";
    case ESRCH: return "ESRCH";
    case EBADF: return "EBADF";
    case ECHILD: return "ECHILD";
    case EACCES: return "EACCES";
    case EFAULT: return "EFAULT";
    case EEXIST: return "EEXIST";
    case ENOTDIR: return "ENOTDIR";
    case EISDIR: return "EISDIR";
    case EINVAL: return "EINVAL";
    case ESPIPE: return "ESPIPE";
    case EPIPE: return "EPIPE";
    case ENOTEMPTY: return "ENOTEMPTY";
    case EXDEV: return "EXDEV";
    case ENOSYS: return "ENOSYS";
    default: return "?";
    }
}

static void show(const char *what, long ret) {
    if (ret < 0)
        printf("%s = -1 (%s, errno %d)\n", what, errname(errno), errno);
    else
        printf("%s = %ld\n", what, ret);
}

/// fd を返す呼び出し:番号は出さない(親から受け継いだ fd があると、番号が環境で変わる)
static void show_fd(const char *what, long ret) {
    if (ret < 0)
        show(what, ret);
    else
        printf("%s = fd\n", what);
}

int main(int argc, char **argv) {
    char dir[64];
    // 他と重ならない作業用ディレクトリ(名前は出さない)
    snprintf(dir, sizeof dir, "/tmp/compat-%d", (int)getpid());
    signal(SIGPIPE, SIG_IGN);
    setvbuf(stdout, NULL, _IONBF, 0);

    printf("argc = %d, argv[1] = %s\n", argc, argc > 1 ? argv[1] : "(none)");

    // ファイル
    show("mkdir", mkdir(dir, 0755));
    show("mkdir again", mkdir(dir, 0755));
    if (chdir(dir) != 0) return 1;
    show("open missing", open("missing", O_RDONLY));
    int fd = open("f", O_RDWR | O_CREAT | O_EXCL, 0644);
    show_fd("open create", fd);
    show("open excl", open("f", O_RDWR | O_CREAT | O_EXCL, 0644));
    show("write", write(fd, "hello, world\n", 13));
    show("lseek end", lseek(fd, 0, SEEK_END));
    show("lseek set", lseek(fd, 7, SEEK_SET));
    char buf[64] = {0};
    show("read", read(fd, buf, sizeof buf - 1));
    printf("data = %s", buf);
    show("lseek bad whence", lseek(fd, 0, 99));
    show("lseek negative", lseek(fd, -100, SEEK_SET));
    struct stat st;
    show("fstat", fstat(fd, &st));
    printf("size = %ld, regular = %d, mode = %o\n", (long)st.st_size, S_ISREG(st.st_mode), st.st_mode & 0777);
    show("ftruncate", ftruncate(fd, 5));
    show("stat", stat("f", &st));
    printf("size = %ld\n", (long)st.st_size);
    show("close", close(fd));
    show("close again", close(fd));
    show("read bad fd", read(fd, buf, 1));
    show_fd("read only", (fd = open("f", O_RDONLY)));
    show("write to read-only fd", write(fd, "x", 1));
    close(fd);
    show("open dir for write", open(".", O_WRONLY));
    show("read a directory", (fd = open(".", O_RDONLY | O_DIRECTORY)) < 0 ? -1 : read(fd, buf, 1));
    close(fd);
    show("open file as dir", open("f", O_RDONLY | O_DIRECTORY));
    show("mkdir sub", mkdir("sub", 0700));
    show("rmdir nonempty", (close(open("sub/x", O_CREAT | O_WRONLY, 0600)), rmdir("sub")));
    show("unlink sub/x", unlink("sub/x"));
    show("rmdir sub", rmdir("sub"));
    show("unlink missing", unlink("missing"));

    // rename
    close(open("r1", O_CREAT | O_WRONLY, 0644));
    close(open("r2", O_CREAT | O_WRONLY, 0644));
    show("rename", rename("r1", "r3"));
    show("rename missing", rename("r1", "r4"));
    show("rename over file", rename("r3", "r2"));
    show("mkdir rd", mkdir("rd", 0755));
    show("rename file over dir", rename("r2", "rd"));
    show("rename dir over file", rename("rd", "r2"));
    show("rename into itself", rename("rd", "rd/x"));
    show("rename dir", rename("rd", "rd2"));
    show("rmdir rd2", rmdir("rd2"));
    show("unlink r2", unlink("r2"));

    // fd の複製
    fd = open("f", O_RDONLY);
    show("dup2", dup2(fd, 20));
    show("fcntl getfd", fcntl(20, F_GETFD));
    show("fcntl setfd", fcntl(20, F_SETFD, FD_CLOEXEC));
    show("fcntl getfd", fcntl(20, F_GETFD));
    show("dup3 same fd", dup3(20, 20, 0));
    close(20);
    close(fd);

    // パイプ
    int p[2];
    show("pipe", pipe(p));
    show("write pipe", write(p[1], "abc", 3));
    show("lseek pipe", lseek(p[0], 0, SEEK_SET));
    show("read pipe", read(p[0], buf, sizeof buf));
    close(p[1]);
    show("read pipe eof", read(p[0], buf, sizeof buf));
    show("pipe", pipe(p));
    close(p[0]);
    show("write pipe without reader", write(p[1], "x", 1));
    close(p[1]);

    // プロセス
    pid_t c = fork();
    if (c == 0) _exit(7);
    int status;
    show("wait", waitpid(c, &status, 0) == c);
    printf("exited = %d, code = %d\n", WIFEXITED(status), WEXITSTATUS(status));
    show("wait no child", waitpid(-1, &status, 0));
    show("wait nohang", waitpid(-1, &status, WNOHANG));

    // メモリ
    char *m = mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    show("mmap", m == MAP_FAILED ? -1 : 0);
    printf("zeroed = %d\n", m[0] == 0 && m[8191] == 0);
    m[100] = 42;
    show("munmap", munmap(m, 8192));
    show("mmap bad length", mmap(NULL, 0, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) == MAP_FAILED ? -1 : 0);

    // 時刻
    struct timespec a, b, nap = {0, 20 * 1000 * 1000};
    clock_gettime(CLOCK_MONOTONIC, &a);
    show("nanosleep", nanosleep(&nap, NULL));
    clock_gettime(CLOCK_MONOTONIC, &b);
    long ms = (b.tv_sec - a.tv_sec) * 1000 + (b.tv_nsec - a.tv_nsec) / 1000000;
    printf("slept at least 20ms = %d\n", ms >= 20);
    show("bad clock", clock_gettime(12345, &a));

    // 後片付け
    show("unlink f", unlink("f"));
    show("chdir /", chdir("/"));
    show("rmdir", rmdir(dir));
    return 3;
}
