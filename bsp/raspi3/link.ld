/* Raspberry Pi 3:ファームウェア(と QEMU -M raspi3b)は kernel8.img を 0x8_0000 に置く */
KERNEL_BASE = 0x80000;
INCLUDE kernel.ld
