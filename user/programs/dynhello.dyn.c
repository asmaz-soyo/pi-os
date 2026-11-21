// 動的リンクのプログラム(ファイル名の .dyn で、build.rs が -dynamic でビルドする)。
// カーネルは PT_INTERP の /lib/ld-musl-aarch64.so.1 も読み込み、そこから動き出す。
// 動的リンカが libc の関数(printf など)を結びつけてから main が呼ばれる。
#include <stdio.h>
#include <sys/auxv.h>

int main(void) {
    // 動的リンカが置かれた場所(AT_BASE)。静的リンクなら 0
    unsigned long base = getauxval(AT_BASE);
    printf("Hello from a dynamically linked program (interpreter at %#lx)\n", base);
    return base != 0 ? 0 : 1;
}
