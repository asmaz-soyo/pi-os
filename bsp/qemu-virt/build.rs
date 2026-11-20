fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    // INCLUDE kernel.ld を arch/aarch64 から探させる
    println!("cargo:rustc-link-arg=-L{dir}/../../arch/aarch64");
    println!("cargo:rustc-link-arg=-T{dir}/link.ld");
    println!("cargo:rerun-if-changed=link.ld");
    println!("cargo:rerun-if-changed=../../arch/aarch64/kernel.ld");
}
