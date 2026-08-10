use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

const STATE_SCHEMA: u32 = 1;
const TOKEN_HEADER: &str = "x-willdeep-token";
const SERVER_VERSION_HEADER: &str = "x-willdeep-version";

#[derive(Clone, Debug, Subcommand)]
pub enum DaemonAction {
    /// Start the local Runtime Daemon in the background.
    Start,
    /// Show Runtime Daemon health and endpoint information.
    Status,
    /// Gracefully stop the local Runtime Daemon.
    Stop,
    /// Print Runtime Daemon logs.
    Logs {
        #[arg(long, default_value_t = 100)]
        lines: usize,
        #[arg(short, long)]
        follow: bool,
    },
    /// Internal foreground server entry used by `daemon start`.
    #[command(hide = true)]
    Run,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct DaemonState {
    schema: u32,
    version: String,
    pid: u32,
    address: SocketAddr,
    token: String,
    started_at: u64,
}

#[derive(Clone)]
struct ServerState {
    token: String,
    started_at: u64,
    shutdown: Arc<Notify>,
    events: Arc<EventLog>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct RuntimeEvent {
    sequence: u64,
    timestamp: u64,
    kind: String,
    message: String,
}

struct EventLog {
    path: PathBuf,
    state: Mutex<EventLogState>,
}

struct EventLogState {
    next_sequence: u64,
}

pub async fn handle(action: DaemonAction) -> Result<()> {
    let home = crate::config::willdeep_home()?;
    match action {
        DaemonAction::Start => start(&home).await,
        DaemonAction::Status => status(&home).await,
        DaemonAction::Stop => stop(&home).await,
        DaemonAction::Logs { lines, follow } => logs(&home, lines, follow).await,
        DaemonAction::Run => run(&home).await,
    }
}

pub async fn attach(after: u64) -> Result<()> {
    let home = crate::config::willdeep_home()?;
    let paths = DaemonPaths::new(&home);
    let state = load_state(&paths.state).context("Runtime Daemon is not running")?;
    probe(&state)
        .await
        .context("Runtime Daemon is unavailable")?;
    println!(
        "Attached to WillDeep Runtime at {} after event {}. Ctrl+C detaches without stopping it.",
        state.address, after
    );
    let mut cursor = after;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("Detached at event {cursor}; Runtime continues running.");
                return Ok(());
            }
            _ = tokio::time::sleep(Duration::from_millis(250)) => {
                for event in fetch_events(&state, cursor).await? {
                    println!("{}\t{}\t{}", event.sequence, event.kind, event.message);
                    cursor = cursor.max(event.sequence);
                }
            }
        }
    }
}

pub async fn detach() -> Result<()> {
    let home = crate::config::willdeep_home()?;
    let state =
        load_state(&DaemonPaths::new(&home).state).context("Runtime Daemon is not running")?;
    let health = probe(&state)
        .await
        .context("Runtime Daemon is unavailable")?;
    println!(
        "Client detached; Runtime pid {} continues running (uptime {}s).",
        health.pid, health.uptime_seconds
    );
    Ok(())
}

async fn start(home: &Path) -> Result<()> {
    let paths = DaemonPaths::new(home);
    std::fs::create_dir_all(&paths.directory)?;
    if let Ok(state) = load_state(&paths.state)
        && probe(&state).await.is_ok()
    {
        println!(
            "WillDeep Runtime Daemon is already running (pid {}).",
            state.pid
        );
        return Ok(());
    }
    remove_stale_state(&paths.state)?;
    let stdout = append_log(&paths.log)?;
    let stderr = stdout.try_clone()?;
    let executable = std::env::current_exe().context("resolve current WillDeep executable")?;
    let mut command = Command::new(executable);
    command
        .args(["daemon", "run"])
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    configure_detached(&mut command);
    let child = command.spawn().context("start Runtime Daemon")?;

    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Ok(state) = load_state(&paths.state)
            && probe(&state).await.is_ok()
        {
            println!(
                "WillDeep Runtime Daemon started (pid {}, {}).",
                state.pid, state.address
            );
            return Ok(());
        }
    }
    bail!(
        "Runtime Daemon process {} did not become healthy; inspect {}",
        child.id(),
        paths.log.display()
    )
}

async fn status(home: &Path) -> Result<()> {
    let paths = DaemonPaths::new(home);
    let state = match load_state(&paths.state) {
        Ok(state) => state,
        Err(_) => {
            println!("WillDeep Runtime Daemon is stopped.");
            return Ok(());
        }
    };
    match probe(&state).await {
        Ok(health) => {
            println!(
                "running\tpid={}\taddress={}\tversion={}\tuptime={}s",
                state.pid, state.address, health.version, health.uptime_seconds
            );
            Ok(())
        }
        Err(error) => bail!("Runtime Daemon state is stale (pid {}): {error}", state.pid),
    }
}

async fn stop(home: &Path) -> Result<()> {
    let paths = DaemonPaths::new(home);
    let state = match load_state(&paths.state) {
        Ok(state) => state,
        Err(_) => {
            println!("WillDeep Runtime Daemon is already stopped.");
            return Ok(());
        }
    };
    let response = client()
        .post(format!("http://{}/v1/shutdown", state.address))
        .header(TOKEN_HEADER, &state.token)
        .send()
        .await
        .context("contact Runtime Daemon")?;
    if !response.status().is_success() {
        bail!(
            "Runtime Daemon rejected shutdown: HTTP {}",
            response.status()
        );
    }
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if !paths.state.exists() {
            println!("WillDeep Runtime Daemon stopped.");
            return Ok(());
        }
    }
    bail!("Runtime Daemon acknowledged shutdown but did not exit")
}

async fn logs(home: &Path, lines: usize, follow: bool) -> Result<()> {
    let path = DaemonPaths::new(home).log;
    let mut file = File::open(&path)
        .with_context(|| format!("open Runtime Daemon log at {}", path.display()))?;
    let mut content = String::new();
    file.read_to_string(&mut content)?;
    let tail = content
        .lines()
        .rev()
        .take(lines.max(1))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    if !tail.is_empty() {
        println!("{tail}");
    }
    if !follow {
        return Ok(());
    }
    let mut position = file.seek(SeekFrom::End(0))?;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            _ = tokio::time::sleep(Duration::from_millis(250)) => {
                let length = file.metadata()?.len();
                if length < position {
                    position = 0;
                }
                if length > position {
                    file.seek(SeekFrom::Start(position))?;
                    let mut chunk = String::new();
                    file.read_to_string(&mut chunk)?;
                    print!("{chunk}");
                    position = length;
                }
            }
        }
    }
}

async fn run(home: &Path) -> Result<()> {
    let paths = DaemonPaths::new(home);
    std::fs::create_dir_all(&paths.directory)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("bind Runtime Daemon control endpoint")?;
    let address = listener.local_addr()?;
    let started_at = now();
    let state = DaemonState {
        schema: STATE_SCHEMA,
        version: willdeep_core::VERSION.to_owned(),
        pid: std::process::id(),
        address,
        token: uuid::Uuid::new_v4().simple().to_string(),
        started_at,
    };
    write_state(&paths.state, &state)?;
    let cleanup = StateCleanup {
        path: paths.state.clone(),
        token: state.token.clone(),
    };
    let shutdown_signal = Arc::new(Notify::new());
    let events = Arc::new(EventLog::open(paths.events.clone())?);
    let server_state = Arc::new(ServerState {
        token: state.token,
        started_at,
        shutdown: shutdown_signal.clone(),
        events: events.clone(),
    });
    let app = Router::new()
        .route("/v1/health", get(health))
        .route("/v1/events", get(events_handler))
        .route("/v1/shutdown", post(shutdown_handler))
        .with_state(server_state);
    events.append(
        "daemon.started",
        format!("pid={} address={address}", std::process::id()),
    )?;
    eprintln!(
        "WillDeep Runtime Daemon {} listening on {} (pid {})",
        willdeep_core::VERSION,
        address,
        std::process::id()
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(async move { shutdown_signal.notified().await })
        .await
        .context("run Runtime Daemon control server")?;
    events.append("daemon.stopped", format!("pid={}", std::process::id()))?;
    drop(cleanup);
    eprintln!("WillDeep Runtime Daemon stopped");
    Ok(())
}

async fn health(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let mut response = Json(Health {
        status: "ok".to_owned(),
        version: willdeep_core::VERSION.to_owned(),
        pid: std::process::id(),
        uptime_seconds: now().saturating_sub(state.started_at),
    })
    .into_response();
    response.headers_mut().insert(
        SERVER_VERSION_HEADER,
        HeaderValue::from_static(willdeep_core::VERSION),
    );
    Ok(response)
}

async fn shutdown_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    state.shutdown.notify_one();
    Ok(StatusCode::ACCEPTED.into_response())
}

#[derive(Deserialize)]
struct EventsQuery {
    #[serde(default)]
    after: u64,
    #[serde(default = "default_event_limit")]
    limit: usize,
}

fn default_event_limit() -> usize {
    200
}

async fn events_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let events = state
        .events
        .read_after(query.after, query.limit.clamp(1, 1_000))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut response = Json(events).into_response();
    response.headers_mut().insert(
        SERVER_VERSION_HEADER,
        HeaderValue::from_static(willdeep_core::VERSION),
    );
    Ok(response)
}

fn authorize(state: &ServerState, headers: &HeaderMap) -> Result<(), StatusCode> {
    if headers
        .get(TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        == Some(state.token.as_str())
    {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Health {
    status: String,
    version: String,
    pid: u32,
    uptime_seconds: u64,
}

async fn probe(state: &DaemonState) -> Result<Health> {
    if state.schema != STATE_SCHEMA {
        bail!("unsupported state schema {}", state.schema);
    }
    let response = client()
        .get(format!("http://{}/v1/health", state.address))
        .header(TOKEN_HEADER, &state.token)
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("health endpoint returned HTTP {}", response.status());
    }
    Ok(response.json().await?)
}

async fn fetch_events(state: &DaemonState, after: u64) -> Result<Vec<RuntimeEvent>> {
    let response = client()
        .get(format!(
            "http://{}/v1/events?after={after}&limit=200",
            state.address
        ))
        .header(TOKEN_HEADER, &state.token)
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("events endpoint returned HTTP {}", response.status());
    }
    Ok(response.json().await?)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(1))
        .timeout(Duration::from_secs(2))
        .build()
        .expect("daemon HTTP client")
}

struct DaemonPaths {
    directory: PathBuf,
    state: PathBuf,
    log: PathBuf,
    events: PathBuf,
}

impl DaemonPaths {
    fn new(home: &Path) -> Self {
        let directory = home.join("runtime");
        Self {
            state: directory.join("daemon.json"),
            log: directory.join("daemon.log"),
            events: directory.join("events.ndjson"),
            directory,
        }
    }
}

impl EventLog {
    fn open(path: PathBuf) -> Result<Self> {
        let next_sequence = read_events(&path, 0, usize::MAX)?
            .last()
            .map_or(1, |event| event.sequence.saturating_add(1));
        Ok(Self {
            path,
            state: Mutex::new(EventLogState { next_sequence }),
        })
    }

    fn append(&self, kind: impl Into<String>, message: impl Into<String>) -> Result<RuntimeEvent> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Runtime event log lock poisoned"))?;
        let event = RuntimeEvent {
            sequence: state.next_sequence,
            timestamp: now(),
            kind: kind.into(),
            message: message.into(),
        };
        let mut file = append_log(&self.path)?;
        serde_json::to_writer(&mut file, &event)?;
        std::io::Write::write_all(&mut file, b"\n")?;
        file.sync_data()?;
        state.next_sequence = state.next_sequence.saturating_add(1);
        Ok(event)
    }

    fn read_after(&self, after: u64, limit: usize) -> Result<Vec<RuntimeEvent>> {
        read_events(&self.path, after, limit)
    }
}

fn read_events(path: &Path, after: u64, limit: usize) -> Result<Vec<RuntimeEvent>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(path)?;
    let mut events = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let event: RuntimeEvent = serde_json::from_str(&line)
            .with_context(|| format!("parse Runtime event from {}", path.display()))?;
        if event.sequence > after {
            events.push(event);
            if events.len() >= limit {
                break;
            }
        }
    }
    Ok(events)
}

struct StateCleanup {
    path: PathBuf,
    token: String,
}

impl Drop for StateCleanup {
    fn drop(&mut self) {
        if load_state(&self.path).is_ok_and(|state| state.token == self.token) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn load_state(path: &Path) -> Result<DaemonState> {
    let data = std::fs::read(path)
        .with_context(|| format!("read Runtime Daemon state at {}", path.display()))?;
    serde_json::from_slice(&data).context("parse Runtime Daemon state")
}

fn write_state(path: &Path, state: &DaemonState) -> Result<()> {
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4().simple()));
    let data = serde_json::to_vec_pretty(state)?;
    write_private(&temporary, &data)?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

fn append_log(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    private_options(&mut options);
    options
        .open(path)
        .with_context(|| format!("open Runtime Daemon log at {}", path.display()))
}

fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    private_options(&mut options);
    let mut file = options.open(path)?;
    std::io::Write::write_all(&mut file, data)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn private_options(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn private_options(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn configure_detached(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(windows)]
fn configure_detached(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
}

fn remove_stale_state(path: &Path) -> Result<()> {
    if path.exists() {
        std::fs::remove_file(path)
            .with_context(|| format!("remove stale Runtime Daemon state at {}", path.display()))?;
    }
    Ok(())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_without_exposing_token_in_logs() {
        let root = std::env::temp_dir().join(format!("willdeep-daemon-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("daemon.json");
        let state = DaemonState {
            schema: STATE_SCHEMA,
            version: "1.2.3".to_owned(),
            pid: 42,
            address: "127.0.0.1:9847".parse().unwrap(),
            token: "private-token".to_owned(),
            started_at: 10,
        };
        write_state(&path, &state).unwrap();
        assert_eq!(load_state(&path).unwrap(), state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn authorization_requires_exact_local_token() {
        let event_path = std::env::temp_dir().join(format!(
            "willdeep-daemon-auth-events-{}",
            uuid::Uuid::new_v4()
        ));
        let state = ServerState {
            token: "expected".to_owned(),
            started_at: 0,
            shutdown: Arc::new(Notify::new()),
            events: Arc::new(EventLog::open(event_path).unwrap()),
        };
        assert_eq!(
            authorize(&state, &HeaderMap::new()),
            Err(StatusCode::UNAUTHORIZED)
        );
        let mut headers = HeaderMap::new();
        headers.insert(TOKEN_HEADER, HeaderValue::from_static("expected"));
        assert_eq!(authorize(&state, &headers), Ok(()));
    }

    #[test]
    fn event_log_assigns_sequences_and_resumes_after_cursor() {
        let root = std::env::temp_dir().join(format!("willdeep-events-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("events.ndjson");
        let log = EventLog::open(path.clone()).unwrap();
        assert_eq!(log.append("first", "one").unwrap().sequence, 1);
        assert_eq!(log.append("second", "two").unwrap().sequence, 2);
        assert_eq!(log.read_after(1, 10).unwrap()[0].kind, "second");
        drop(log);

        let reopened = EventLog::open(path).unwrap();
        assert_eq!(reopened.append("third", "three").unwrap().sequence, 3);
        assert_eq!(reopened.read_after(0, 2).unwrap().len(), 2);
        std::fs::remove_dir_all(root).unwrap();
    }
}
