// madvise:MADV_DONTNEED のあとの無名のページは 0 に戻る(jemalloc はこれを当てにして
// ページを使い回す)。範囲の確かめ方(EINVAL)も Linux と比べる。
// マップされていないページを含む範囲は、Linux では ENOMEM だが qemu-user は 0 を返すので、
// ここでは比べない(カーネルのホストのテストで確かめる)。
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define PAGE 4096

static void show(const char *what, int r) {
    printf("%s: %d %s\n", what, r, r == 0 ? "ok" : strerror(errno));
}

int main(void) {
    char *a = mmap(NULL, 3 * PAGE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (a == MAP_FAILED) return 1;
    strcpy(a + 10, "first");
    strcpy(a + PAGE + 10, "second");
    strcpy(a + 2 * PAGE + 10, "third");

    // 長さはページの単位に切り上げる:PAGE + 1 → 2 ページ
    show("dontneed 2 pages", madvise(a, PAGE + 1, MADV_DONTNEED));
    printf("after: [%s] [%s] [%s]\n", a + 10, a + PAGE + 10, a + 2 * PAGE + 10);
    strcpy(a + 10, "reused");
    printf("reused: [%s]\n", a + 10);

    show("unaligned", madvise(a + 1, PAGE, MADV_DONTNEED));
    show("zero length", madvise(a, 0, MADV_DONTNEED));

    char *ro = mmap(NULL, PAGE, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    show("prot_none", madvise(ro, PAGE, MADV_DONTNEED));
    show("willneed", madvise(a, PAGE, MADV_WILLNEED));
    return 0;
}
