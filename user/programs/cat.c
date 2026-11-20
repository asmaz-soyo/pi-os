// ファイル(なければ標準入力)の中身を標準出力に写す
#include <fcntl.h>
#include <stdio.h>
#include <unistd.h>

static int copy(int fd) {
    char buf[4096];
    ssize_t n;
    while ((n = read(fd, buf, sizeof buf)) > 0) {
        if (write(1, buf, n) != n) return 1;
    }
    return n < 0;
}

int main(int argc, char **argv) {
    if (argc < 2) return copy(0);
    int status = 0;
    for (int i = 1; i < argc; i++) {
        int fd = open(argv[i], O_RDONLY);
        if (fd < 0) {
            perror(argv[i]);
            status = 1;
            continue;
        }
        status |= copy(fd);
        close(fd);
    }
    return status;
}
