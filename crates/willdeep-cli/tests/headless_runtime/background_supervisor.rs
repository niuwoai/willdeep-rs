use super::*;

#[test]
fn background_supervisor_completes_work_and_kills_it_when_parent_disconnects() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).expect("create test home");
    std::fs::create_dir_all(&workspace).expect("create test Workspace");

    let rejected = willdeep(&home)
        .args(["daemon", "background-supervisor"])
        .env_remove("WILLDEEP_INTERNAL_BACKGROUND_SUPERVISOR")
        .output()
        .expect("invoke untrusted background supervisor");
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("background supervisor is an internal command")
    );

    let mut completed = spawn_background_supervisor(&home);
    let completed_liveness = send_supervisor_request(
        &mut completed,
        serde_json::json!({
            "command": supervisor_print_command(),
            "sandbox": { "policy": "Off", "writable_roots": [] },
            "workspace": workspace,
            "timeout_seconds": 10
        }),
    );
    let completed = completed
        .wait_with_output()
        .expect("wait for completed background supervisor");
    drop(completed_liveness);
    assert_success(&completed, "completed background supervisor");
    let completed: serde_json::Value =
        serde_json::from_slice(&completed.stdout).expect("parse completed supervisor result");
    assert_eq!(completed["status"], "completed");
    assert!(
        completed["output"]
            .as_str()
            .is_some_and(|value| value.contains("supervisor-ok"))
    );

    let mut disconnected = spawn_background_supervisor(&home);
    let child_pid_path = workspace.join("supervisor-child.pid");
    let disconnected_liveness = send_supervisor_request(
        &mut disconnected,
        serde_json::json!({
            "command": supervisor_wait_command(),
            "sandbox": { "policy": "Off", "writable_roots": [] },
            "workspace": workspace,
            "timeout_seconds": 60
        }),
    );
    // 等不到 PID 文件时带上 supervisor 的输出，分得清是慢还是命令本身出错。
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !child_pid_path.exists() {
        if std::time::Instant::now() >= deadline {
            drop(disconnected_liveness);
            let output = disconnected
                .wait_with_output()
                .expect("collect background supervisor output");
            panic!(
                "supervised command wrote no PID file within 5s; supervisor exited {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Windows 上照样读取并解析 PID（证明子进程确实起来了），只是不查它是否还活着。
    #[cfg_attr(not(unix), allow(unused_variables))]
    let child_pid = std::fs::read_to_string(&child_pid_path)
        .expect("read supervised child PID")
        .parse::<u32>()
        .expect("parse supervised child PID");
    let started = std::time::Instant::now();
    drop(disconnected_liveness);
    let disconnected = disconnected
        .wait_with_output()
        .expect("wait for disconnected background supervisor");
    assert_success(&disconnected, "disconnected background supervisor");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "parent disconnect must not wait for the 30 second Shell command"
    );
    let disconnected: serde_json::Value =
        serde_json::from_slice(&disconnected.stdout).expect("parse disconnected result");
    assert_eq!(disconnected["status"], "killed");
    #[cfg(unix)]
    wait_until(Duration::from_secs(5), || !process_exists(child_pid));
    std::fs::remove_dir_all(root).expect("remove supervisor test root");
}

#[cfg(unix)]
#[test]
fn background_supervisor_applies_read_only_sandbox() {
    let _serial = process_test_guard();
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let mut child = spawn_background_supervisor(&home);
    let liveness = send_supervisor_request(
        &mut child,
        serde_json::json!({
            "command": "touch sandbox-must-not-write",
            "workspace": workspace,
            "timeout_seconds": 10,
            "sandbox": { "policy": "ReadOnly", "writable_roots": [] }
        }),
    );
    let output = child.wait_with_output().unwrap();
    drop(liveness);
    assert_success(&output, "read-only supervisor result");
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_ne!(result["status"], "completed");
    assert!(!workspace.join("sandbox-must-not-write").exists());
    std::fs::remove_dir_all(root).unwrap();
}

fn spawn_background_supervisor(home: &Path) -> std::process::Child {
    willdeep(home)
        .args(["daemon", "background-supervisor"])
        .env("WILLDEEP_INTERNAL_BACKGROUND_SUPERVISOR", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn background supervisor")
}

fn send_supervisor_request(
    child: &mut std::process::Child,
    request: serde_json::Value,
) -> std::process::ChildStdin {
    let payload = serde_json::to_vec(&request).expect("serialize supervisor request");
    let mut input = child.stdin.take().expect("background supervisor stdin");
    input
        .write_all(&u32::try_from(payload.len()).unwrap().to_be_bytes())
        .expect("write supervisor request length");
    input.write_all(&payload).expect("write supervisor request");
    input.flush().expect("flush supervisor request");
    input
}

#[cfg(unix)]
fn supervisor_print_command() -> &'static str {
    "printf supervisor-ok"
}

#[cfg(windows)]
fn supervisor_print_command() -> &'static str {
    "Write-Output 'supervisor-ok'"
}

#[cfg(unix)]
fn supervisor_wait_command() -> &'static str {
    "sleep 30 & child=$!; printf '%s' \"$child\" > supervisor-child.pid; wait \"$child\""
}

#[cfg(windows)]
fn supervisor_wait_command() -> &'static str {
    "$child = Start-Process powershell.exe -ArgumentList '-NoProfile','-Command','Start-Sleep -Seconds 30' -PassThru; Set-Content -Path supervisor-child.pid -Value $child.Id -NoNewline; Wait-Process -Id $child.Id"
}

#[cfg(unix)]
fn process_exists(pid: u32) -> bool {
    std::process::Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}
