// /dev/vda(virtio-blk のディスク)のセクタ 1 にある目印と、MBR の印を読む
#include <fcntl.h>
#include <string.h>
#include <unistd.h>

int main(void) {
    int fd = open("/dev/vda", O_RDWR);
    if (fd < 0) return 1;
    char buf[16];
    if (pread(fd, buf, 15, 512) != 15 || memcmp(buf, "Pi-OS test disk", 15)) return 2;
    unsigned char sig[2];
    if (pread(fd, sig, 2, 510) != 2 || sig[0] != 0x55 || sig[1] != 0xaa) return 3;
    // 書いて読み戻す(MBR と ext2 のパーティションの間のすき間にあるセクタ 2)
    off_t last = 2 * 512;
    if (pwrite(fd, "written", 7, last) != 7) return 4;
    if (pread(fd, buf, 7, last) != 7 || memcmp(buf, "written", 7)) return 5;
    close(fd);
    return 0;
}
