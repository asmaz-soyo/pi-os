// musl の printf と stdio を使う最初の C プログラム
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(int argc, char **argv) {
    printf("Hello from musl! argc=%d argv[0]=%s\n", argc, argv[0]);
    FILE *f = fopen("/etc/motd", "r");
    if (!f) {
        perror("fopen /etc/motd");
        return 1;
    }
    char line[128];
    while (fgets(line, sizeof line, f)) {
        fputs(line, stdout);
    }
    fclose(f);
    char *big = malloc(1 << 20);   // 大きな確保は mmap になる
    if (!big) {
        puts("malloc failed");
        return 2;
    }
    memset(big, 'x', 1 << 20);
    free(big);
    puts("malloc ok");
    return 0;
}
