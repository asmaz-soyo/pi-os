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
    ENXIO = 6,
    E2BIG = 7,
    ENOEXEC = 8,
    EBADF = 9,
    ECHILD = 10,
    EAGAIN = 11,
    ENOMEM = 12,
    EACCES = 13,
    EFAULT = 14,
    EEXIST = 17,
    EXDEV = 18,
    ENOTDIR = 20,
    EISDIR = 21,
    EINVAL = 22,
    EMFILE = 24,
    ENOTTY = 25,
    EFBIG = 27,
    ENOSPC = 28,
    ESPIPE = 29,
    EROFS = 30,
    EPIPE = 32,
    ERANGE = 34,
    ENAMETOOLONG = 36,
    ENOSYS = 38,
    ENOTEMPTY = 39,
    ELOOP = 40,
    ENOTSOCK = 88,
    EPROTONOSUPPORT = 93,
    ESOCKTNOSUPPORT = 94,
    EOPNOTSUPP = 95,
    EAFNOSUPPORT = 97,
    EADDRINUSE = 98,
    EISCONN = 106,
    ENOTCONN = 107,
    ECONNREFUSED = 111,
}

impl Errno {
    /// システムコールの戻り値(負の errno を u64 にしたもの)。
    pub fn as_syscall_return(self) -> u64 {
        (-(self as i64)) as u64
    }
}

/// errno の名前(トレースの表示用)。
pub fn name(errno: i32) -> Option<&'static str> {
    Some(match errno {
        1 => "EPERM",
        2 => "ENOENT",
        3 => "ESRCH",
        4 => "EINTR",
        5 => "EIO",
        6 => "ENXIO",
        7 => "E2BIG",
        8 => "ENOEXEC",
        9 => "EBADF",
        10 => "ECHILD",
        11 => "EAGAIN",
        12 => "ENOMEM",
        13 => "EACCES",
        14 => "EFAULT",
        17 => "EEXIST",
        18 => "EXDEV",
        20 => "ENOTDIR",
        21 => "EISDIR",
        22 => "EINVAL",
        24 => "EMFILE",
        25 => "ENOTTY",
        27 => "EFBIG",
        28 => "ENOSPC",
        29 => "ESPIPE",
        30 => "EROFS",
        32 => "EPIPE",
        34 => "ERANGE",
        36 => "ENAMETOOLONG",
        38 => "ENOSYS",
        39 => "ENOTEMPTY",
        40 => "ELOOP",
        88 => "ENOTSOCK",
        93 => "EPROTONOSUPPORT",
        94 => "ESOCKTNOSUPPORT",
        95 => "EOPNOTSUPP",
        97 => "EAFNOSUPPORT",
        98 => "EADDRINUSE",
        106 => "EISCONN",
        107 => "ENOTCONN",
        111 => "ECONNREFUSED",
        _ => return None,
    })
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
