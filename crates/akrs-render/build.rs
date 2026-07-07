//! 构建脚本：在编译时获取 git commit 数量作为 Build 号。
//!
//! 运行 `git rev-list --count HEAD` 获取当前 HEAD 的提交总数，
//! 并通过 `BUILD_NUMBER` 环境变量传递给编译器。
//! 若 git 不可用或不在 git 仓库中，回退到 "0"。
//!
//! 在代码中使用 `env!("BUILD_NUMBER")` 读取此值。

fn main() {
    let build_number = std::process::Command::new("git")
        .args(["rev-list", "--count", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "0".to_string());

    println!("cargo:rustc-env=BUILD_NUMBER={}", build_number);
    println!("cargo:rerun-if-changed=../../.git/HEAD");
}
