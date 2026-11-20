// /boot(ディスクの FAT32 パーティション)を読む。長い名前とサブディレクトリ、読み取り専用を確かめる。
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int main(void) {
    char buf[128] = {0};
    int fd = open("/boot/README.TXT", O_RDONLY);
    if (fd < 0 || read(fd, buf, sizeof buf - 1) <= 0) return 1;
    close(fd);
    if (strcmp(buf, "This file lives on the FAT32 boot partition of /dev/vda.\n")) return 2;

    // 長い名前(config.txt は 8.3 に小文字があるので LFN で書かれる)
    FILE *f = fopen("/boot/config.txt", "r");
    if (!f) return 3;
    int found = 0;
    while (fgets(buf, sizeof buf, f))
        if (!strcmp(buf, "arm_64bit=1\n")) found = 1;
    fclose(f);
    if (!found) return 4;

    DIR *d = opendir("/boot/overlays");
    if (!d) return 5;
    struct dirent *e;
    int seen = 0;
    while ((e = readdir(d)))
        if (!strcmp(e->d_name, "disable-bt.dtbo")) seen = 1;
    closedir(d);
    if (!seen) return 6;

    // 書き込めない
    if (open("/boot/new.txt", O_WRONLY | O_CREAT, 0644) >= 0 || errno != EROFS) return 7;
    if (open("/boot/config.txt", O_WRONLY) >= 0 || errno != EROFS) return 8;
    printf("boot partition OK\n");
    return 0;
}
