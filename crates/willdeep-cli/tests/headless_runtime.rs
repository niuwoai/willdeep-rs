use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

const MOCK_REPLY: &str = "headless runtime reply";

#[test]
fn run_uses_persistent_runtime_and_continues_the_session() {
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).expect("create test home");
    std::fs::create_dir_all(&workspace).expect("create test workspace");
    let provider = MockProvider::start();
    let config = root.join("config.toml");
    write_private_config(&config, provider.api_base());
    let mut guard = TestGuard::new(root.clone(), home.clone());

    let first = willdeep(&home)
        .args([
            "run",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "--output",
            "json",
            "first runtime turn",
        ])
        .output()
        .expect("run first headless turn");
    assert_success(&first, "first headless turn");
    let first_json: serde_json::Value =
        serde_json::from_slice(&first.stdout).expect("parse first completion JSON");
    assert_eq!(first_json["type"], "completed");
    assert_eq!(first_json["text"], MOCK_REPLY);
    let session_id = first_json["session_id"]
        .as_str()
        .expect("completion session id");

    let second = willdeep(&home)
        .args([
            "run",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "--session",
            session_id,
            "--output",
            "json",
            "second runtime turn",
        ])
        .output()
        .expect("continue headless Session");
    assert_success(&second, "continued headless turn");
    let second_json: serde_json::Value =
        serde_json::from_slice(&second.stdout).expect("parse second completion JSON");
    assert_eq!(second_json["session_id"], session_id);
    assert_eq!(second_json["text"], MOCK_REPLY);

    let turns = willdeep(&home)
        .args(["session", "turns", session_id])
        .output()
        .expect("list persistent Runtime turns");
    assert_success(&turns, "list Runtime turns");
    let turn_lines = String::from_utf8(turns.stdout)
        .expect("turn output is UTF-8")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    assert_eq!(turn_lines, 2, "both turns must be durable Runtime records");
    assert_eq!(provider.requests(), 2, "each turn must reach the Provider");

    guard.stop_daemon();
}

#[test]
fn runtime_provider_failure_preserves_the_documented_exit_code() {
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).expect("create test home");
    std::fs::create_dir_all(&workspace).expect("create test workspace");
    let provider = MockProvider::start_with_status(503);
    let config = root.join("config.toml");
    write_private_config(&config, provider.api_base());
    let _guard = TestGuard::new(root, home.clone());

    let output = willdeep(&home)
        .args([
            "run",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "provider failure",
        ])
        .output()
        .expect("run failing Provider turn");

    assert_eq!(
        output.status.code(),
        Some(3),
        "Provider failures returned through Runtime must keep exit code 3; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(provider.requests(), 1);
}

#[test]
fn public_api_spawns_and_waits_for_a_read_only_child_agent() {
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).expect("create test home");
    std::fs::create_dir_all(&workspace).expect("create test workspace");
    let provider = MockProvider::start_waiting_root();
    let config = root.join("config.toml");
    write_private_config(&config, provider.api_base());
    let mut session = willdeep_core::Session::new(workspace.clone(), None, "external spawn");
    session.config = Some(config.clone());
    willdeep_core::SessionStore::new(&home)
        .save(&mut session)
        .expect("persist Core Session fixture");
    let _guard = TestGuard::new(root.clone(), home.clone());

    let root_turn = willdeep(&home)
        .args([
            "run",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "--session",
            &session.id.to_string(),
            "wait for user input",
        ])
        .output()
        .expect("start waiting root Agent");
    assert_eq!(
        root_turn.status.code(),
        Some(4),
        "ask_user must leave the Runtime Turn waiting; stderr:\n{}",
        String::from_utf8_lossy(&root_turn.stderr)
    );

    let editor_params = root.join("editor-spawn.json");
    write_json(
        &editor_params,
        serde_json::json!({
            "session_id": session.id,
            "prompt": "edit a file",
            "profile": "editor"
        }),
    );
    let editor = willdeep(&home)
        .args([
            "api",
            "agent.spawn",
            "--params-file",
            path_text(&editor_params),
        ])
        .output()
        .expect("reject external editor spawn");
    assert!(!editor.status.success());
    let editor_envelope: serde_json::Value =
        serde_json::from_slice(&editor.stdout).expect("parse editor rejection envelope");
    assert_eq!(editor_envelope["error"]["code"], "invalid_request");

    let spawn_params = root.join("scout-spawn.json");
    write_json(
        &spawn_params,
        serde_json::json!({
            "session_id": session.id,
            "prompt": "inspect the repository structure",
            "profile": "scout",
            "label": "external scout"
        }),
    );
    let spawn = willdeep(&home)
        .args([
            "api",
            "agent.spawn",
            "--params-file",
            path_text(&spawn_params),
        ])
        .output()
        .expect("spawn external scout");
    assert_success(&spawn, "spawn external scout");
    let spawn_envelope: serde_json::Value =
        serde_json::from_slice(&spawn.stdout).expect("parse spawn envelope");
    assert_eq!(spawn_envelope["data"]["status"], "queued");
    assert_eq!(spawn_envelope["data"]["profile"], "scout");
    let child_id = spawn_envelope["data"]["id"]
        .as_str()
        .expect("spawned Agent ID");

    let wait_params = root.join("agent-wait.json");
    write_json(
        &wait_params,
        serde_json::json!({"id": child_id, "timeout_ms": 10_000}),
    );
    let wait = willdeep(&home)
        .args([
            "api",
            "agent.wait",
            "--params-file",
            path_text(&wait_params),
        ])
        .output()
        .expect("wait for external scout");
    assert_success(&wait, "wait for external scout");
    let wait_envelope: serde_json::Value =
        serde_json::from_slice(&wait.stdout).expect("parse Agent wait envelope");
    assert_eq!(wait_envelope["data"]["id"], child_id);
    assert_eq!(wait_envelope["data"]["status"], "completed");
    assert_eq!(wait_envelope["data"]["label"], "external scout");
    assert_eq!(
        provider.requests(),
        2,
        "root and child must each reach Provider"
    );
}

#[test]
fn daemon_upgrade_drains_active_work_and_hands_off_without_loss() {
    let root = temporary_root();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).expect("create test home");
    std::fs::create_dir_all(&workspace).expect("create test workspace");
    let provider = MockProvider::start_delayed(Duration::from_secs(3));
    let config = root.join("config.toml");
    write_private_config(&config, provider.api_base());
    let _guard = TestGuard::new(root, home.clone());

    let active = willdeep(&home)
        .args([
            "run",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "active during handoff",
        ])
        .spawn()
        .expect("start active Runtime turn");
    wait_until(Duration::from_secs(10), || provider.requests() == 1);

    let before = daemon_status(&home);
    let old_pid = status_value(&before, "pid");
    let upgrade = willdeep(&home)
        .args(["daemon", "upgrade", "--force", "--timeout", "15"])
        .spawn()
        .expect("start Runtime handoff");
    wait_until(Duration::from_secs(5), || {
        daemon_status(&home).starts_with("draining\t")
    });

    let rejected = willdeep(&home)
        .args([
            "run",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "must wait for replacement runtime",
        ])
        .output()
        .expect("submit while draining");
    assert!(
        !rejected.status.success(),
        "draining Runtime must reject new work"
    );
    assert_eq!(
        provider.requests(),
        1,
        "rejected work must not reach Provider"
    );

    let active = active
        .wait_with_output()
        .expect("wait for active Runtime turn");
    assert_success(&active, "active turn during Runtime handoff");
    let upgrade = upgrade
        .wait_with_output()
        .expect("wait for Runtime handoff");
    assert_success(&upgrade, "Runtime handoff");
    let after = daemon_status(&home);
    assert!(after.starts_with("running\t"));
    assert_ne!(status_value(&after, "pid"), old_pid);

    let replacement = willdeep(&home)
        .args([
            "run",
            "--config",
            path_text(&config),
            "--workspace",
            path_text(&workspace),
            "replacement runtime turn",
        ])
        .output()
        .expect("run turn on replacement Runtime");
    assert_success(&replacement, "replacement Runtime turn");
    assert_eq!(provider.requests(), 2);
}

fn willdeep(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_willdeep"));
    command
        .env("WILLDEEP_HOME", home)
        .env_remove("WILLDEEP_API_BASE")
        .env_remove("WILLDEEP_API_KEY")
        .env_remove("WILLDEEP_CONFIG")
        .env_remove("WILLDEEP_LANGUAGE")
        .env_remove("WILLDEEP_MODEL");
    command
}

fn assert_success(output: &Output, operation: &str) {
    assert!(
        output.status.success(),
        "{operation} failed with {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write_private_config(path: &Path, api_base: String) {
    let contents = format!(
        r#"version = 1
default_provider = "mock"

[agent]
max_turns = 4
approval = "smart"

[providers.mock]
provider = "openai-compatible"
api = "chat-completions"
api_base = "{api_base}"
api_key = "integration-test-only"
model = "mock-model"
"#
    );
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .expect("create private test config")
        .write_all(contents.as_bytes())
        .expect("write test config");
}

fn write_json(path: &Path, value: serde_json::Value) {
    std::fs::write(
        path,
        serde_json::to_vec(&value).expect("serialize test JSON"),
    )
    .expect("write test JSON");
}

fn path_text(path: &Path) -> &str {
    path.to_str().expect("test path is UTF-8")
}

fn temporary_root() -> PathBuf {
    #[cfg(unix)]
    let base = PathBuf::from("/tmp");
    #[cfg(not(unix))]
    let base = std::env::temp_dir();
    base.join(format!("wdhl-{}", uuid::Uuid::new_v4().simple()))
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if condition() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("condition was not met within {timeout:?}");
}

fn daemon_status(home: &Path) -> String {
    let output = willdeep(home)
        .args(["daemon", "status"])
        .output()
        .expect("query Runtime status");
    assert_success(&output, "query Runtime status");
    String::from_utf8(output.stdout)
        .expect("Runtime status is UTF-8")
        .trim()
        .to_owned()
}

fn status_value<'a>(status: &'a str, name: &str) -> &'a str {
    status
        .split_whitespace()
        .find_map(|part| part.strip_prefix(&format!("{name}=")))
        .unwrap_or_else(|| panic!("missing {name} in Runtime status: {status}"))
}

struct TestGuard {
    root: PathBuf,
    home: PathBuf,
    daemon_stopped: bool,
}

impl TestGuard {
    fn new(root: PathBuf, home: PathBuf) -> Self {
        Self {
            root,
            home,
            daemon_stopped: false,
        }
    }

    fn stop_daemon(&mut self) {
        let _ = willdeep(&self.home).args(["daemon", "stop"]).output();
        self.daemon_stopped = true;
    }
}

impl Drop for TestGuard {
    fn drop(&mut self) {
        if !self.daemon_stopped {
            let _ = willdeep(&self.home).args(["daemon", "stop"]).output();
        }
        if thread::panicking() {
            eprintln!(
                "preserving failed Headless Runtime test at {}",
                self.root.display()
            );
            return;
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct MockProvider {
    address: std::net::SocketAddr,
    stop: Arc<AtomicBool>,
    requests: Arc<AtomicUsize>,
    thread: Option<thread::JoinHandle<()>>,
}

impl MockProvider {
    fn start() -> Self {
        Self::start_with_mode(MockMode::Success)
    }

    fn start_with_status(status: u16) -> Self {
        Self::start_with_mode(MockMode::Status(status))
    }

    fn start_waiting_root() -> Self {
        Self::start_with_mode(MockMode::WaitThenSuccess)
    }

    fn start_delayed(delay: Duration) -> Self {
        Self::start_with_mode(MockMode::DelayedSuccess(delay))
    }

    fn start_with_mode(mode: MockMode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock Provider");
        listener
            .set_nonblocking(true)
            .expect("configure mock Provider");
        let address = listener.local_addr().expect("mock Provider address");
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicUsize::new(0));
        let worker_stop = Arc::clone(&stop);
        let worker_requests = Arc::clone(&requests);
        let thread = thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        if !read_request(&mut stream) {
                            continue;
                        }
                        let index = worker_requests.fetch_add(1, Ordering::Relaxed);
                        write_response(&mut stream, mode, index);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept mock Provider request: {error}"),
                }
            }
        });
        Self {
            address,
            stop,
            requests,
            thread: Some(thread),
        }
    }

    fn api_base(&self) -> String {
        format!("http://{}/v1", self.address)
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::Relaxed)
    }
}

#[derive(Clone, Copy)]
enum MockMode {
    Success,
    Status(u16),
    WaitThenSuccess,
    DelayedSuccess(Duration),
}

impl Drop for MockProvider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("join mock Provider");
        }
    }
}

fn read_request(stream: &mut TcpStream) -> bool {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set Provider read timeout");
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        let read = match stream.read(&mut chunk) {
            Ok(read) => read,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::ConnectionReset
                ) =>
            {
                return false;
            }
            Err(error) => panic!("read Provider request: {error}"),
        };
        if read == 0 {
            return false;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(position) = find_bytes(&bytes, b"\r\n\r\n") {
            break position + 4;
        }
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    assert!(headers.starts_with("POST /v1/chat/completions HTTP/1.1"));
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while bytes.len() < header_end + content_length {
        let read = match stream.read(&mut chunk) {
            Ok(read) => read,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::ConnectionReset
                ) =>
            {
                return false;
            }
            Err(error) => panic!("read Provider body: {error}"),
        };
        if read == 0 {
            return false;
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    true
}

fn write_response(stream: &mut TcpStream, mode: MockMode, request_index: usize) {
    if let MockMode::DelayedSuccess(delay) = mode {
        thread::sleep(delay);
    }
    let status = match mode {
        MockMode::Status(status) => status,
        MockMode::Success | MockMode::WaitThenSuccess | MockMode::DelayedSuccess(_) => 200,
    };
    let (reason, body) = if mode_is_waiting_root(mode, request_index) {
        (
            "OK",
            r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"ask_root","type":"function","function":{"name":"ask_user","arguments":"{\"question\":\"keep the root active?\",\"options\":[\"yes\"]}"}}]},"finish_reason":"tool_calls"}]}"#.to_owned(),
        )
    } else if status == 200 {
        (
            "OK",
            format!(
                r#"{{"choices":[{{"message":{{"content":"{MOCK_REPLY}","tool_calls":[]}},"finish_reason":"stop"}}],"usage":{{"prompt_tokens":5,"completion_tokens":3,"total_tokens":8}}}}"#
            ),
        )
    } else {
        (
            "Service Unavailable",
            r#"{"error":"temporary failure"}"#.to_owned(),
        )
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .expect("write Provider response");
}

fn mode_is_waiting_root(mode: MockMode, request_index: usize) -> bool {
    matches!(mode, MockMode::WaitThenSuccess) && request_index == 0
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
