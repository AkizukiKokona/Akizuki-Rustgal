//! 构建脚本：在编译时获取 git commit 数量作为 Build 号。
//!
//! 运行 `git rev-list --count HEAD` 获取当前 HEAD 的提交总数，
//! 并通过 `BUILD_NUMBER` 环境变量传递给编译器。
//! 若 git 不可用或不在 git 仓库中，回退到 "0"。
//!
//! 在代码中使用 `env!("BUILD_NUMBER")` 读取此值。
//!
//! # Build 号基数
//!
//! 本仓库 main 分支的 git 历史曾做过压缩/重置，commit 数从 1 重新计数。
//! 为了让 Build 号与项目实际迭代进度（含历史提交）对齐，这里在 commit 数
//! 基础上叠加一个基数 `BUILD_BASE`。基数取 37，使得在 main 分支 commit 数
//! 为 3 时，Build 号显示为 40（即 `3 + 37 = 40`）。
//!
//! 后续每次提交 commit 数 +1，Build 号同步 +1，无需再手动调整。

/// Build 号基数。叠加在 git commit 数之上，用于对齐项目历史迭代进度。
/// 详见模块文档说明。
const BUILD_BASE: u64 = 37;

fn main() {
    let commit_count: u64 = std::process::Command::new("git")
        .args(["rev-list", "--count", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout)
                    .ok()
                    .and_then(|s| s.trim().parse::<u64>().ok())
            } else {
                None
            }
        })
        .unwrap_or(0);

    let build_number = commit_count + BUILD_BASE;

    println!("cargo:rustc-env=BUILD_NUMBER={}", build_number);
    println!("cargo:rerun-if-changed=../../.git/HEAD");
}
