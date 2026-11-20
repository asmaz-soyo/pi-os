//! ビルドした時刻を埋め込む(RTC がない Pi 3 の CLOCK_REALTIME の初期値)。
//! 再現できるビルドのために、SOURCE_DATE_EPOCH があればそれを使う。

fn main() {
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    let epoch = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        });
    println!("cargo:rustc-env=PIOS_BUILD_EPOCH={epoch}");
}
