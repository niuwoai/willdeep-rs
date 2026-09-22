/// 主线程栈大小，与 Linux / macOS 的缺省值一致。
const MAIN_THREAD_STACK_BYTES: u32 = 8 * 1024 * 1024;

fn main() {
    println!("cargo:rerun-if-changed=../../web/dist");

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
