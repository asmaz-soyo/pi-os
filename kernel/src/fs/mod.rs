//! ファイルシステム。

pub mod devfs;
pub mod epoll;
pub mod ext2;
pub mod fat32;
pub mod initramfs;
pub mod jbd;
pub mod pipe;
pub mod procfs;
pub mod tar;
pub mod timerfd;
pub mod tmpfs;
pub mod vfs;

/// マウントしたものの記録:(デバイス、マウント先、種類)。/proc/mounts が見せる
static MOUNTS: crate::sync::SpinLock<
    alloc::vec::Vec<(
        alloc::string::String,
        alloc::string::String,
        alloc::string::String,
    )>,
> = crate::sync::SpinLock::new(alloc::vec::Vec::new());

/// マウントしたことを記録する(同じマウント先は置き換える)
pub fn record_mount(source: &str, target: &str, kind: &str) {
    use alloc::string::String;
    let mut m = MOUNTS.lock();
    m.retain(|(_, t, _)| t != target);
    m.push((
        String::from(source),
        String::from(target),
        String::from(kind),
    ));
}

/// マウントの一覧:(デバイス、マウント先、種類)
pub fn mounts() -> alloc::vec::Vec<(
    alloc::string::String,
    alloc::string::String,
    alloc::string::String,
)> {
    MOUNTS.lock().clone()
}
