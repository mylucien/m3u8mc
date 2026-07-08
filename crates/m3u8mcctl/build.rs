//! 编译期把 git commit / 构建时间注入为环境变量，供 main.rs 里 `env!()` 读取，
//! 拼进 `--version` 输出。目的：部署时能直接用 `m3u8mcd --version` 确认跑的是
//! 哪个 commit 编译出来的二进制，而不用去猜 CI 有没有用到旧缓存。
//!
//! 优先读 GitHub Actions 提供的 GITHUB_SHA（CI 构建时最可靠，不依赖 checkout
//! 深度）；本地手动 `cargo build` 时没有这个环境变量，回退到 `git rev-parse`；
//! 如果两者都不可用（比如从不带 .git 的源码包构建），兜底写 "unknown"，
//! 不让构建因为拿不到 git 信息而失败。

use std::process::Command;

fn main() {
    // 仓库根目录相对本 crate 是上两级；.git/HEAD 变化时触发重新运行本脚本，
    // 保证每次实际提交变化都能反映到新二进制里（而不是被 cargo 缓存住旧的注入值）。
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");

    let git_sha = std::env::var("GITHUB_SHA")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(8).collect::<String>())
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "--short=8", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|| "unknown".to_string());

    // 本地存在未提交的改动时打个 -dirty 标记，避免把"改过但没提交"的构建
    // 误认成跟某个干净 commit 完全一致。
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    let sha_display = if dirty && git_sha != "unknown" {
        format!("{git_sha}-dirty")
    } else {
        git_sha
    };

    // 构建时间：build.rs 永远在宿主机上执行（即便是交叉编译到其他 target），
    // 所以直接 shell out 到宿主机的 `date` 拿 UTC 时间，不额外引入 chrono 依赖。
    let build_time = Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=M3U8MC_GIT_SHA={sha_display}");
    println!("cargo:rustc-env=M3U8MC_BUILD_TIME={build_time}");
}
