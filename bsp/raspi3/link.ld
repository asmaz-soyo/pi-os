/* Raspberry Pi 3:ファームウェア(と QEMU -M raspi3b)は kernel8.img を 0x8_0000 に置く */
KERNEL_BASE = 0x80000;
/* 0x3F00_0000 からはペリフェラル(UART など) */
BOOT_DEVICE_START = 0x3F000000;
INCLUDE kernel.ld
