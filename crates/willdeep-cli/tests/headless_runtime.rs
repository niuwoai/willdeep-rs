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
        Self::start_with_status(200)
    }

    fn start_with_status(status: u16) -> Self {
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
                        read_request(&mut stream);
                        worker_requests.fetch_add(1, Ordering::Relaxed);
                        write_response(&mut stream, status);
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

impl Drop for MockProvider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("join mock Provider");
        }
    }
}

fn read_request(stream: &mut TcpStream) {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set Provider read timeout");
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut chunk).expect("read Provider request");
        assert!(read > 0, "Provider connection closed before headers");
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
        let read = stream.read(&mut chunk).expect("read Provider body");
        assert!(read > 0, "Provider connection closed before body");
        bytes.extend_from_slice(&chunk[..read]);
    }
}

fn write_response(stream: &mut TcpStream, status: u16) {
    let (reason, body) = if status == 200 {
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

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
