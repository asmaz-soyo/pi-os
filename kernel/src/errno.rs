//! エラー番号。値は Linux(AArch64)と同じにする。

/// カーネル内部のエラー。`Result<T, Errno>` で使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Errno {
    EPERM = 1,
    ENOENT = 2,
    ESRCH = 3,
    EINTR = 4,
    EIO = 5,
    ENOEXEC = 8,
    EBADF = 9,
    ECHILD = 10,
    EAGAIN = 11,
    ENOMEM = 12,
    EACCES = 13,
    EFAULT = 14,
    EEXIST = 17,
    ENOTDIR = 20,
    EISDIR = 21,
    EINVAL = 22,
    EMFILE = 24,
    ENOTTY = 25,
    ESPIPE = 29,
    ERANGE = 34,
    ENOSYS = 38,
}

impl Errno {
    /// システムコールの戻り値(負の errno を u64 にしたもの)。
    pub fn as_syscall_return(self) -> u64 {
        (-(self as i64)) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errno_values_match_linux() {
        assert_eq!(Errno::EPERM as i32, 1);
        assert_eq!(Errno::ENOENT as i32, 2);
        assert_eq!(Errno::EBADF as i32, 9);
        assert_eq!(Errno::ENOMEM as i32, 12);
        assert_eq!(Errno::EFAULT as i32, 14);
        assert_eq!(Errno::EINVAL as i32, 22);
        assert_eq!(Errno::ENOSYS as i32, 38);
    }

    #[test]
    fn syscall_return_value_is_negative_errno() {
        assert_eq!(Errno::ENOSYS.as_syscall_return(), -38i64 as u64);
    }
}
