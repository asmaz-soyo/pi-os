// /mnt(virtio-blk のディスクの ext2 パーティション)のファイルを読む
#include <dirent.h>
#include <stdio.h>
#include <string.h>

int main(void) {
    char line[128] = {0};
    FILE *f = fopen("/mnt/README.txt", "r");
    if (!f || !fgets(line, sizeof line, f)) return 1;
    fclose(f);
    if (strcmp(line, "This file lives on the ext2 partition of /dev/vda.\n")) return 2;
    f = fopen("/mnt/docs/notes.txt", "r");
    if (!f || !fgets(line, sizeof line, f)) return 3;
    fclose(f);
    int found = 0;
    DIR *d = opendir("/mnt");
    for (struct dirent *e; d && (e = readdir(d));) found += !strcmp(e->d_name, "docs");
    if (d) closedir(d);
    return found == 1 ? 0 : 4;
}
