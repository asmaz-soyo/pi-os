// ルート(virtio-blk のディスクの ext2 パーティション)のファイルを読み書きする
#include <dirent.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char line[128] = {0};
    FILE *f = fopen("/README.txt", "r");
    if (!f || !fgets(line, sizeof line, f)) return 1;
    fclose(f);
    if (strcmp(line, "This file lives on the ext2 partition of /dev/vda.\n")) return 2;
    f = fopen("/docs/notes.txt", "r");
    if (!f || !fgets(line, sizeof line, f)) return 3;
    fclose(f);
    int found = 0;
    DIR *d = opendir("/");
    for (struct dirent *e; d && (e = readdir(d));) found += !strcmp(e->d_name, "docs");
    if (d) closedir(d);
    if (found != 1) return 4;

    // 書き込み:ファイルを作って読み戻し、ディレクトリを作って消す
    f = fopen("/written.txt", "w");
    if (!f) return 5;
    for (int i = 0; i < 200; i++) fprintf(f, "line %03d of a file written to ext2\n", i);
    fclose(f);
    f = fopen("/written.txt", "r");
    int lines = 0;
    while (f && fgets(line, sizeof line, f)) lines++;
    if (f) fclose(f);
    if (lines != 200) return 6;
    if (mkdir("/newdir", 0755) != 0) return 7;
    if (rmdir("/newdir") != 0 || unlink("/written.txt") != 0) return 8;
    return 0;
}
