use std::path::{Path, PathBuf};
use std::process::Command;

/// 主线程栈大小，与 Linux / macOS 的缺省值一致。
const MAIN_THREAD_STACK_BYTES: u32 = 8 * 1024 * 1024;

/// 工作区根目录（相对本 crate）。
const WORKSPACE_ROOT: &str = "../..";

/// 决定二进制内容的路径：这些路径下有未提交的改动（含未跟踪文件），构建
/// commit 就带 `-dirty`。改动它们也让本脚本重跑，`-dirty` 不会停在旧值上。
const SOURCE_PATHS: [&str; 4] = ["crates", "web/src", "Cargo.toml", "Cargo.lock"];

fn main() {
    println!("cargo:rerun-if-changed=../../web/dist");
    emit_build_commit();

    // Windows 主线程缺省栈只有 1 MiB：`#[tokio::main]` 在主线程上 block_on 的
    // 顶层 future 很大，`willdeep run` 一进 headless 路径就栈溢出
    // （0xC00000FD）。Linux / macOS 主线程是 8 MiB，这里把 Windows 对齐。
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" {
        let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
        if target_env == "msvc" {
            println!("cargo:rustc-link-arg-bins=/STACK:{MAIN_THREAD_STACK_BYTES}");
        } else {
            println!("cargo:rustc-link-arg-bins=-Wl,--stack,{MAIN_THREAD_STACK_BYTES}");
        }
    }
}

/// 把构建时的完整 commit 写进 `WILLDEEP_BUILD_COMMIT`（`willdeep prompt check`
/// 会打印它，对照评测据此核对二进制与仓库是同一份代码）。不在 git 仓库里构建
/// 时不设置，程序里显示为 `unknown`。
fn emit_build_commit() {
    // 从源码包构建时，上两级目录可能落在别的仓库里：顶层对不上就不认。
    let root = Path::new(WORKSPACE_ROOT).canonicalize().ok();
    let toplevel = git(&["rev-parse", "--show-toplevel"])
        .and_then(|path| Path::new(&path).canonicalize().ok());
    if root.is_none() || root != toplevel {
        return;
    }
    let Some(commit) = git(&["rev-parse", "HEAD"]) else {
        return;
    };
    for path in SOURCE_PATHS {
        println!("cargo:rerun-if-changed={WORKSPACE_ROOT}/{path}");
    }
    // 切分支、提交都会改 HEAD 或它指向的引用；工作树里 `.git` 可能是个文件，
    // 所以按 git 自己给的目录找。
    if let Some(git_dir) = git_path("--git-dir") {
        println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
    }
    if let Some(common_dir) = git_path("--git-common-dir") {
        println!(
            "cargo:rerun-if-changed={}",
            common_dir.join("packed-refs").display()
        );
        if let Some(reference) = git(&["symbolic-ref", "-q", "HEAD"]) {
            println!(
                "cargo:rerun-if-changed={}",
                common_dir.join(reference).display()
            );
        }
    }
    let mut status = vec!["status", "--porcelain", "--untracked-files=normal", "--"];
    status.extend(SOURCE_PATHS);
    let dirty = Command::new("git")
        .current_dir(WORKSPACE_ROOT)
        .args(&status)
        .output()
        .map(|output| !output.status.success() || !output.stdout.is_empty())
        .unwrap_or(true);
    let suffix = if dirty { "-dirty" } else { "" };
    println!("cargo:rustc-env=WILLDEEP_BUILD_COMMIT={commit}{suffix}");
}

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .current_dir(WORKSPACE_ROOT)
        .args(args)
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (output.status.success() && !text.is_empty()).then_some(text)
}

/// `git rev-parse <flag>` 给出的目录，相对路径按工作区根目录解析。
fn git_path(flag: &str) -> Option<PathBuf> {
    let path = PathBuf::from(git(&["rev-parse", flag])?);
    Some(if path.is_absolute() {
        path
    } else {
        Path::new(WORKSPACE_ROOT).join(path)
    })
}
