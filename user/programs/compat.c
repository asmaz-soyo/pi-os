// システムコールの互換テスト(cargo xtask test --compat)。
// Linux(qemu-aarch64)と Pi-OS の両方で動かし、出力が同じになることを確かめる。
// 環境で変わるもの(uid、時刻、パス、プロセス番号)は出さない。
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/un.h>
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
    case ELOOP: return "ELOOP";
    case ENXIO: return "ENXIO";
    case ENOSYS: return "ENOSYS";
    case EPROTONOSUPPORT: return "EPROTONOSUPPORT";
    case ESOCKTNOSUPPORT: return "ESOCKTNOSUPPORT";
    case EOPNOTSUPP: return "EOPNOTSUPP";
    case EAFNOSUPPORT: return "EAFNOSUPPORT";
    case ENOTSOCK: return "ENOTSOCK";
    case EADDRINUSE: return "EADDRINUSE";
    case EISCONN: return "EISCONN";
    case ENOTCONN: return "ENOTCONN";
    case ECONNREFUSED: return "ECONNREFUSED";
    case EAGAIN: return "EAGAIN";
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

    // 名前付きパイプ
    show("mkfifo", mkfifo("ff", 0644));
    show("mkfifo exists", mkfifo("ff", 0644));
    show("open fifo writer nonblock", open("ff", O_WRONLY | O_NONBLOCK));
    int fr = open("ff", O_RDONLY | O_NONBLOCK);
    show_fd("open fifo reader", fr);
    int fw = open("ff", O_WRONLY);
    show_fd("open fifo writer", fw);
    show("fifo write", write(fw, "fifo", 4));
    show("fifo read", read(fr, buf, sizeof buf));
    close(fw);
    show("fifo eof", read(fr, buf, sizeof buf));
    close(fr);
    stat("ff", &st);
    printf("is fifo = %d\n", S_ISFIFO(st.st_mode));
    show("unlink ff", unlink("ff"));
    show("fchdir", (fd = open(".", O_RDONLY | O_DIRECTORY)) < 0 ? -1 : fchdir(fd));
    close(fd);

    // O_APPEND
    fd = open("ap", O_CREAT | O_WRONLY, 0644);
    write(fd, "one\n", 4);
    close(fd);
    fd = open("ap", O_WRONLY | O_APPEND);
    lseek(fd, 0, SEEK_SET);
    show("append write", write(fd, "two\n", 4));
    show("append flag", fcntl(fd, F_GETFL) & O_APPEND ? 1 : 0);
    close(fd);
    stat("ap", &st);
    printf("size after append = %ld\n", (long)st.st_size);
    show("unlink ap", unlink("ap"));

    // ハードリンク
    close(open("h1", O_CREAT | O_WRONLY, 0644));
    show("link", link("h1", "h2"));
    stat("h1", &st);
    printf("nlink = %ld\n", (long)st.st_nlink);
    show("link exists", link("h1", "h2"));
    show("link missing", link("nope", "h3"));
    show("unlink h1", unlink("h1"));
    stat("h2", &st);
    printf("nlink = %ld\n", (long)st.st_nlink);
    show("unlink h2", unlink("h2"));

    // chmod
    close(open("cm", O_CREAT | O_WRONLY, 0644));
    show("chmod", chmod("cm", 0600));
    stat("cm", &st);
    printf("mode = %o\n", st.st_mode & 07777);
    fd = open("cm", O_RDONLY);
    show("fchmod", fchmod(fd, 04755));
    fstat(fd, &st);
    printf("mode = %o\n", st.st_mode & 07777);
    close(fd);
    show("chmod missing", chmod("nope", 0600));
    show("unlink cm", unlink("cm"));

    // シンボリックリンク
    close(open("t", O_CREAT | O_WRONLY, 0644));
    show("symlink", symlink("t", "ln"));
    show("symlink exists", symlink("t", "ln"));
    char lb[16] = {0};
    show("readlink", readlink("ln", lb, sizeof lb - 1));
    printf("target = %s\n", lb);
    show("readlink short", readlink("ln", lb, 0) );
    show("readlink not a link", readlink("t", lb, sizeof lb));
    struct stat ls;
    show("lstat", lstat("ln", &ls));
    printf("is link = %d, size = %ld\n", S_ISLNK(ls.st_mode), (long)ls.st_size);
    show("stat through link", stat("ln", &ls));
    printf("is regular = %d\n", S_ISREG(ls.st_mode));
    show_fd("open through link", (fd = open("ln", O_RDONLY)));
    close(fd);
    show("open nofollow", open("ln", O_RDONLY | O_NOFOLLOW));
    show("symlink loop", symlink("loop", "loop"));
    show("open loop", open("loop", O_RDONLY));
    show("symlink dangling", symlink("nowhere", "dang"));
    show("open dangling", open("dang", O_RDONLY));
    show("unlink ln", unlink("ln"));
    show("unlink loop", unlink("loop"));
    show("unlink dang", unlink("dang"));
    show("unlink t", unlink("t"));

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

    // ソケット(AF_UNIX)
    int sv[2];
    show("socketpair", socketpair(AF_UNIX, SOCK_STREAM, 0, sv));
    show("write sock", write(sv[0], "ping", 4));
    show("read sock", read(sv[1], buf, sizeof buf));
    show("write sock back", write(sv[1], "pong!", 5));
    show("read sock back", read(sv[0], buf, sizeof buf));
    struct stat sst;
    fstat(sv[0], &sst);
    printf("is socket = %d\n", S_ISSOCK(sst.st_mode));
    show("lseek sock", lseek(sv[0], 0, SEEK_SET));
    close(sv[0]);
    show("read sock eof", read(sv[1], buf, sizeof buf));
    show("write sock closed", write(sv[1], "x", 1));
    close(sv[1]);
    show("socketpair bad type", socketpair(AF_UNIX, 6, 0, sv));
    show("socketpair bad proto", socketpair(AF_UNIX, SOCK_STREAM, 7, sv));
    show("socketpair bad family", socketpair(99, SOCK_STREAM, 0, sv));

    // 名前でつなぐソケット(AF_UNIX)
    struct sockaddr_un sa = {.sun_family = AF_UNIX}, ca = {.sun_family = AF_UNIX};
    strcpy(sa.sun_path, "srv");
    int srv = socket(AF_UNIX, SOCK_STREAM, 0);
    show_fd("socket", srv);
    show("bind", bind(srv, (struct sockaddr *)&sa, sizeof sa));
    show("bind again", bind(srv, (struct sockaddr *)&sa, sizeof sa));
    struct stat sk;
    stat("srv", &sk);
    printf("bound is socket = %d\n", S_ISSOCK(sk.st_mode));
    show("open socket file", open("srv", O_RDONLY));
    int cli = socket(AF_UNIX, SOCK_STREAM, 0);
    show("connect not listening", connect(cli, (struct sockaddr *)&sa, sizeof sa));
    show("accept not listening", accept(srv, NULL, NULL));
    show("listen", listen(srv, 4));
    show("write unconnected", write(cli, "x", 1));
    show("read unconnected", read(cli, buf, sizeof buf));
    show("getpeername unconnected", getpeername(cli, (struct sockaddr *)&ca, &(socklen_t){sizeof ca}));
    show("connect", connect(cli, (struct sockaddr *)&sa, sizeof sa));
    show("connect again", connect(cli, (struct sockaddr *)&sa, sizeof sa));
    socklen_t alen = sizeof ca;
    int acc = accept(srv, (struct sockaddr *)&ca, &alen);
    show_fd("accept", acc);
    printf("peer len = %d, family = %d\n", (int)alen, ca.sun_family);
    alen = sizeof ca;
    show("getpeername", getpeername(cli, (struct sockaddr *)&ca, &alen));
    printf("peer = %s, len = %d\n", ca.sun_path, (int)alen);
    alen = sizeof ca;
    show("getsockname", getsockname(acc, (struct sockaddr *)&ca, &alen));
    printf("name = %s, len = %d\n", ca.sun_path, (int)alen);
    show("write to server", write(cli, "hello", 5));
    show("read at server", read(acc, buf, sizeof buf));
    show("shutdown wr", shutdown(cli, SHUT_WR));
    show("read after shutdown", read(acc, buf, sizeof buf));
    show("write after shutdown", write(cli, "x", 1));
    show("write back", write(acc, "ok", 2));
    show("read back", read(cli, buf, sizeof buf));
    show("bind in use", bind(socket(AF_UNIX, SOCK_STREAM, 0), (struct sockaddr *)&sa, sizeof sa));
    show("bind not socket", bind(1, (struct sockaddr *)&sa, sizeof sa));
    int nb = socket(AF_UNIX, SOCK_STREAM | SOCK_NONBLOCK, 0);
    struct sockaddr_un ab = {.sun_family = AF_UNIX};
    snprintf(ab.sun_path + 1, sizeof ab.sun_path - 1, "compat-%d", (int)getpid());
    socklen_t ablen = offsetof(struct sockaddr_un, sun_path) + 1 + strlen(ab.sun_path + 1);
    show("bind abstract", bind(nb, (struct sockaddr *)&ab, ablen));
    show("listen abstract", listen(nb, 1));
    show("accept nothing yet", accept(nb, NULL, NULL));
    close(acc);
    close(cli);
    close(srv);
    close(nb);
    cli = socket(AF_UNIX, SOCK_STREAM, 0);
    show("connect closed server", connect(cli, (struct sockaddr *)&sa, sizeof sa));
    strcpy(sa.sun_path, "none");
    show("connect missing", connect(cli, (struct sockaddr *)&sa, sizeof sa));
    close(cli);
    show("unlink srv", unlink("srv"));

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
