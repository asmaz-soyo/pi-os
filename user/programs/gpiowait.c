// /dev/gpio/N で GPIO の割り込みを 1 回待つ(QEMU virt では N = 3 が電源ボタン)
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

int main(int argc, char **argv) {
    const char *line = argc > 1 ? argv[1] : "3";
    char path[32];
    snprintf(path, sizeof path, "/dev/gpio/%s", line);
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        perror(path);
        return 1;
    }
    printf("waiting for GPIO %s\n", line);
    fflush(stdout);
    uint64_t n;
    if (read(fd, &n, sizeof n) != sizeof n) return 2;
    printf("GPIO %s: %llu event(s)\n", line, (unsigned long long)n);
    return 0;
}
