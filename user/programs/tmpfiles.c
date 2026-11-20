// /tmp(tmpfs)で、ファイルの作成・読み書き・ディレクトリの一覧・削除ができるかを確かめる。
#include <dirent.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    FILE *f = fopen("/tmp/hello.txt", "w");
    if (!f) return 1;
    fprintf(f, "written by pid %d\n", getpid());
    fclose(f);

    char line[64] = {0};
    f = fopen("/tmp/hello.txt", "r");
    if (!f || !fgets(line, sizeof line, f)) return 2;
    fclose(f);
    if (strncmp(line, "written by pid", 14) != 0) return 3;

    if (mkdir("/tmp/dir", 0755) != 0) return 4;
    int seen = 0;
    DIR *d = opendir("/tmp");
    if (!d) return 5;
    for (struct dirent *e; (e = readdir(d));) {
        if (!strcmp(e->d_name, "hello.txt") || !strcmp(e->d_name, "dir")) seen++;
    }
    closedir(d);
    if (seen != 2) return 6;

    if (unlink("/tmp/hello.txt") != 0 || rmdir("/tmp/dir") != 0) return 7;
    if (access("/tmp/hello.txt", F_OK) == 0) return 8;
    return 0;
}
