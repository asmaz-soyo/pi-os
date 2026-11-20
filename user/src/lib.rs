//! カーネルに埋め込むユーザープログラム(`programs/` を `build.rs` がビルドしたもの)。
//! M5 で initramfs(tar)に置き換える。
#![no_std]

macro_rules! program {
    ($name:ident, $file:literal, $doc:literal) => {
        #[doc = $doc]
        pub static $name: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/", $file));
    };
}

program!(
    HELLO,
    "hello",
    "「Hello from user space!」を出して終了ステータス 0 で終わる"
);
program!(EXIT42, "exit42", "終了ステータス 42 で終わるだけ");
program!(FAULT, "fault", "アドレス 0 を読んでフォールトする");
program!(
    EFAULT,
    "efault",
    "不正なポインタで write を呼び、返ってきた errno を終了ステータスにする"
);
