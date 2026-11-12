//! `--probe`：一次性诊断（内容在 `diag.rs`，GUI 的「诊断」按钮用的是同一份）。

pub fn run() {
    print!("{}", crate::diag::text());
}
