//! 把 Runtime Daemon 作为脱离调用方的常驻进程拉起。

use std::process::Command;

use anyhow::Result;

#[cfg(unix)]
pub(super) fn configure_detached(command: &mut Command) -> Result<()> {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
    Ok(())
}

/// Windows 上 `CreateProcess` 以 `bInheritHandles=TRUE` 拉起子进程，父进程里**所有**
/// 可继承句柄都会复制过去——包括调用方交给 `willdeep run` 的 stdout / stderr 管道，
/// 哪怕 Daemon 自己的 stdio 已经重定向到日志文件。常驻的 Daemon 攥着管道写端，
/// 捕获输出的调用方（CI 脚本、`subprocess.run(capture_output=True)`、`| jq`）就永远
/// 等不到 EOF。拉起之前先把本进程三个标准句柄的继承标志摘掉；之后用
/// `Stdio::inherit` 的子进程不受影响，标准库每次 spawn 都另外复制一份可继承句柄。
#[cfg(windows)]
pub(super) fn configure_detached(command: &mut Command) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    stop_standard_handle_inheritance()?;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    Ok(())
}

#[cfg(windows)]
fn stop_standard_handle_inheritance() -> Result<()> {
    use std::os::windows::io::{AsRawHandle, RawHandle};

    use anyhow::Context;

    const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
    const INVALID_HANDLE_VALUE: isize = -1;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetHandleInformation(handle: RawHandle, mask: u32, flags: u32) -> i32;
    }

    let handles = [
        ("stdin", std::io::stdin().as_raw_handle()),
        ("stdout", std::io::stdout().as_raw_handle()),
        ("stderr", std::io::stderr().as_raw_handle()),
    ];
    for (name, handle) in handles {
        // 没有控制台也没被重定向的进程（例如 GUI 父进程拉起）拿到的是空句柄，无可继承。
        if handle.is_null() || handle as isize == INVALID_HANDLE_VALUE {
            continue;
        }
        // SAFETY: 句柄来自本进程的标准流，在进程存活期间有效；只改继承标志，不关闭、
        // 不转移所有权。
        let changed = unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) };
        if changed == 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!("stop the Runtime Daemon from inheriting the caller's {name} handle")
            });
        }
    }
    Ok(())
}
