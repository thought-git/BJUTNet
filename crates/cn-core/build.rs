//! 让 cargo 知道"版本号是构建时注入的环境变量"，改了就得重编。
//!
//! 不写这个的话：`CAMPUSNET_VERSION=1.0.1 cargo build` 会复用旧的缓存产物，
//! 二进制里的版本号还是上一次的（实测踩到）。
fn main() {
    println!("cargo:rerun-if-env-changed=CAMPUSNET_VERSION");
    println!("cargo:rerun-if-env-changed=CAMPUSNET_BUILD");
}
