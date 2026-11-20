/* QEMU -M virt:RAM は 0x4000_0000 から。Linux の慣習に合わせて 0x8_0000 ずらして置く */
KERNEL_BASE = 0x40080000;
INCLUDE kernel.ld
