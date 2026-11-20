// /proc を読んで、自分の名前とメモリの情報が見えるかを確かめる
#include <stdio.h>
#include <string.h>

static int contains(const char *path, const char *needle) {
    FILE *f = fopen(path, "r");
    if (!f) return 0;
    char buf[1024];
    size_t n = fread(buf, 1, sizeof buf - 1, f);
    fclose(f);
    buf[n] = 0;
    return strstr(buf, needle) != NULL;
}

int main(void) {
    if (!contains("/proc/self/status", "Name:\tprocread\n")) return 1;
    if (!contains("/proc/meminfo", "MemTotal:")) return 2;
    if (!contains("/proc/uptime", ".")) return 3;
    return 0;
}
