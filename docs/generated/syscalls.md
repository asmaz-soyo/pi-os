<!-- cargo xtask docs で生成。手で編集しない -->
# 実装済みのシステムコール

番号は Linux AArch64 と同じ。表の元は `kernel/src/syscall/mod.rs` の `TABLE`。

| 番号 | 名前 | 引数の数 |
| --- | --- | --- |
| 17 | `getcwd` | 2 |
| 29 | `ioctl` | 3 |
| 56 | `openat` | 4 |
| 57 | `close` | 1 |
| 62 | `lseek` | 3 |
| 63 | `read` | 3 |
| 64 | `write` | 3 |
| 65 | `readv` | 3 |
| 66 | `writev` | 3 |
| 79 | `newfstatat` | 4 |
| 80 | `fstat` | 2 |
| 93 | `exit` | 1 |
| 94 | `exit_group` | 1 |
| 96 | `set_tid_address` | 1 |
| 113 | `clock_gettime` | 2 |
| 123 | `sched_getaffinity` | 3 |
| 134 | `rt_sigaction` | 4 |
| 135 | `rt_sigprocmask` | 4 |
| 160 | `uname` | 1 |
| 172 | `getpid` | 0 |
| 174 | `getuid` | 0 |
| 175 | `geteuid` | 0 |
| 176 | `getgid` | 0 |
| 177 | `getegid` | 0 |
| 178 | `gettid` | 0 |
| 214 | `brk` | 1 |
| 215 | `munmap` | 2 |
| 216 | `mremap` | 5 |
| 222 | `mmap` | 6 |
| 226 | `mprotect` | 3 |
| 233 | `madvise` | 3 |
| 278 | `getrandom` | 3 |

合計 32 個
