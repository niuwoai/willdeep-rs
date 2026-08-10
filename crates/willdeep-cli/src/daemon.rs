use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use axum::extract::{Path as AxumPath, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::{Mutex as AsyncMutex, Notify, RwLock};

const STATE_SCHEMA: u32 = 1;
const TOKEN_HEADER: &str = "x-willdeep-token";
const SERVER_VERSION_HEADER: &str = "x-willdeep-version";
const LOCK_STALE_AFTER_SECONDS: u64 = 10;

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
    /// Submit a non-interactive Harness task to the persistent Runtime.
    Submit {
        /// Workspace root available to the task.
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Provider profile from config.toml.
        #[arg(long)]
        profile: Option<String>,
        /// TOML configuration path inherited by the task.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Prompt sent through private stdin rather than process arguments.
        #[arg(value_name = "PROMPT", num_args = 1.., trailing_var_arg = true)]
        prompt: Vec<String>,
    },
    /// List tasks owned by the Runtime.
    Tasks,
    /// Show one Runtime-owned task.
    Task { id: uuid::Uuid },
    /// Request cancellation of a Runtime-owned task.
    Cancel { id: uuid::Uuid },
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

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DaemonLock {
    token: String,
    created_at: u64,
}

#[derive(Clone)]
struct ServerState {
    token: String,
    started_at: u64,
    shutdown: Arc<Notify>,
    events: Arc<EventLog>,
    tasks: Arc<TaskManager>,
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

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RuntimeTaskStatus {
    Queued,
    Running,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct RuntimeTask {
    id: uuid::Uuid,
    status: RuntimeTaskStatus,
    workspace: PathBuf,
    profile: Option<String>,
    pid: Option<u32>,
    created_at: u64,
    started_at: Option<u64>,
    completed_at: Option<u64>,
    exit_code: Option<i32>,
    error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SubmitTask {
    prompt: String,
    workspace: PathBuf,
    profile: Option<String>,
    config: Option<PathBuf>,
}

struct TaskManager {
    path: PathBuf,
    executable: PathBuf,
    events: Arc<EventLog>,
    tasks: RwLock<HashMap<uuid::Uuid, RuntimeTask>>,
    persistence: AsyncMutex<()>,
    cancellations: Mutex<HashMap<uuid::Uuid, Arc<Notify>>>,
}

pub async fn handle(action: DaemonAction) -> Result<()> {
    let home = crate::config::willdeep_home()?;
    match action {
        DaemonAction::Start => start(&home).await,
        DaemonAction::Status => status(&home).await,
        DaemonAction::Stop => stop(&home).await,
        DaemonAction::Logs { lines, follow } => logs(&home, lines, follow).await,
        DaemonAction::Submit {
            workspace,
            profile,
            config,
            prompt,
        } => submit(&home, workspace, profile, config, prompt).await,
        DaemonAction::Tasks => list_tasks(&home).await,
        DaemonAction::Task { id } => show_task(&home, id).await,
        DaemonAction::Cancel { id } => cancel_task(&home, id).await,
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

async fn submit(
    home: &Path,
    workspace: Option<PathBuf>,
    profile: Option<String>,
    config: Option<PathBuf>,
    prompt: Vec<String>,
) -> Result<()> {
    let prompt = prompt.join(" ");
    if prompt.trim().is_empty() {
        bail!("Runtime task prompt must not be empty");
    }
    let workspace = workspace
        .unwrap_or(std::env::current_dir()?)
        .canonicalize()?;
    let state = ensure_running(home).await?;
    let response = client()
        .post(format!("http://{}/v1/tasks", state.address))
        .header(TOKEN_HEADER, &state.token)
        .json(&SubmitTask {
            prompt,
            workspace,
            profile,
            config,
        })
        .send()
        .await?;
    if !response.status().is_success() {
        bail!(
            "Runtime rejected task submission: {}",
            response.text().await?
        );
    }
    let task: RuntimeTask = response.json().await?;
    println!(
        "submitted\tid={}\tstatus={:?}\tworkspace={}",
        task.id,
        task.status,
        task.workspace.display()
    );
    Ok(())
}

async fn list_tasks(home: &Path) -> Result<()> {
    let state = ensure_running(home).await?;
    let tasks: Vec<RuntimeTask> = authorized_get(&state, "/v1/tasks").await?;
    for task in tasks {
        print_task(&task);
    }
    Ok(())
}

async fn show_task(home: &Path, id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    let task: RuntimeTask = authorized_get(&state, &format!("/v1/tasks/{id}")).await?;
    print_task(&task);
    Ok(())
}

async fn cancel_task(home: &Path, id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    let response = client()
        .post(format!("http://{}/v1/tasks/{id}/stop", state.address))
        .header(TOKEN_HEADER, &state.token)
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("Runtime rejected cancellation: {}", response.text().await?);
    }
    let task: RuntimeTask = response.json().await?;
    print_task(&task);
    Ok(())
}

async fn ensure_running(home: &Path) -> Result<DaemonState> {
    let path = DaemonPaths::new(home).state;
    if let Ok(state) = load_state(&path)
        && probe(&state).await.is_ok()
    {
        return Ok(state);
    }
    start(home).await?;
    load_state(&path)
}

async fn authorized_get<T: serde::de::DeserializeOwned>(
    state: &DaemonState,
    path: &str,
) -> Result<T> {
    let response = client()
        .get(format!("http://{}{}", state.address, path))
        .header(TOKEN_HEADER, &state.token)
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("Runtime request failed: {}", response.text().await?);
    }
    Ok(response.json().await?)
}

fn print_task(task: &RuntimeTask) {
    println!(
        "{}\t{:?}\tpid={}\texit={}\t{}",
        task.id,
        task.status,
        task.pid
            .map_or_else(|| "-".to_owned(), |pid| pid.to_string()),
        task.exit_code
            .map_or_else(|| "-".to_owned(), |code| code.to_string()),
        task.workspace.display()
    );
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
    let lock = match acquire_daemon_lock(&paths.lock) {
        Ok(lock) => lock,
        Err(error) => {
            for _ in 0..50 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                if let Ok(state) = load_state(&paths.state)
                    && probe(&state).await.is_ok()
                {
                    println!(
                        "WillDeep Runtime Daemon is already running (pid {}).",
                        state.pid
                    );
                    return Ok(());
                }
            }
            return Err(error).context("another Runtime Daemon start is in progress");
        }
    };
    let mut lock_cleanup = OwnedLockCleanup::new(paths.lock.clone(), lock.token.clone());
    let stdout = append_log(&paths.log)?;
    let stderr = stdout.try_clone()?;
    let executable = std::env::current_exe().context("resolve current WillDeep executable")?;
    let mut command = Command::new(executable);
    command
        .args(["daemon", "run"])
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .env("WILLDEEP_DAEMON_LOCK_TOKEN", &lock.token);
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
            lock_cleanup.disarm();
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
    let lock_token = std::env::var("WILLDEEP_DAEMON_LOCK_TOKEN")
        .context("daemon run is an internal command requiring an acquired lock")?;
    let lock = load_daemon_lock(&paths.lock)?;
    if lock.token != lock_token {
        bail!("Runtime Daemon lock ownership changed before startup");
    }
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
        lock_path: paths.lock.clone(),
        lock_token,
    };
    let shutdown_signal = Arc::new(Notify::new());
    let events = Arc::new(EventLog::open(paths.events.clone())?);
    let tasks = Arc::new(TaskManager::open(
        paths.tasks.clone(),
        std::env::current_exe()?,
        events.clone(),
    )?);
    let server_state = Arc::new(ServerState {
        token: state.token,
        started_at,
        shutdown: shutdown_signal.clone(),
        events: events.clone(),
        tasks: tasks.clone(),
    });
    let app = Router::new()
        .route("/v1/health", get(health))
        .route("/v1/events", get(events_handler))
        .route("/v1/tasks", get(tasks_handler).post(submit_task_handler))
        .route("/v1/tasks/{id}", get(task_handler))
        .route("/v1/tasks/{id}/stop", post(stop_task_handler))
        .route("/v1/shutdown", post(shutdown_handler))
        .layer(middleware::from_fn(server_version_header))
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
    tasks.cancel_all().await;
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

async fn server_version_header(request: Request, next: middleware::Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        SERVER_VERSION_HEADER,
        HeaderValue::from_static(willdeep_core::VERSION),
    );
    response
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

async fn tasks_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    Ok(Json(state.tasks.list().await).into_response())
}

async fn task_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<uuid::Uuid>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    state
        .tasks
        .get(id)
        .await
        .map(|task| Json(task).into_response())
        .ok_or(StatusCode::NOT_FOUND)
}

async fn submit_task_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(request): Json<SubmitTask>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let task = state.tasks.submit(request).await.map_err(|error| {
        eprintln!("Runtime task submission failed: {error:#}");
        StatusCode::BAD_REQUEST
    })?;
    Ok((StatusCode::ACCEPTED, Json(task)).into_response())
}

async fn stop_task_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<uuid::Uuid>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    state
        .tasks
        .cancel(id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .map(IntoResponse::into_response)
        .ok_or(StatusCode::NOT_FOUND)
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
    tasks: PathBuf,
    lock: PathBuf,
}

impl DaemonPaths {
    fn new(home: &Path) -> Self {
        let directory = home.join("runtime");
        Self {
            state: directory.join("daemon.json"),
            log: directory.join("daemon.log"),
            events: directory.join("events.ndjson"),
            tasks: directory.join("tasks.json"),
            lock: directory.join("daemon.lock"),
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

impl TaskManager {
    fn open(path: PathBuf, executable: PathBuf, events: Arc<EventLog>) -> Result<Self> {
        let mut tasks = load_tasks(&path)?;
        let mut recovered = false;
        for task in tasks.values_mut() {
            if matches!(
                task.status,
                RuntimeTaskStatus::Queued
                    | RuntimeTaskStatus::Running
                    | RuntimeTaskStatus::Cancelling
            ) {
                task.status = RuntimeTaskStatus::Interrupted;
                task.completed_at = Some(now());
                task.error = Some("Runtime restarted while task was active".to_owned());
                recovered = true;
            }
        }
        if recovered {
            persist_tasks(&path, &tasks)?;
        }
        Ok(Self {
            path,
            executable,
            events,
            tasks: RwLock::new(tasks),
            persistence: AsyncMutex::new(()),
            cancellations: Mutex::new(HashMap::new()),
        })
    }

    async fn list(&self) -> Vec<RuntimeTask> {
        let mut tasks = self
            .tasks
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        tasks.sort_by_key(|task| std::cmp::Reverse(task.created_at));
        tasks
    }

    async fn get(&self, id: uuid::Uuid) -> Option<RuntimeTask> {
        self.tasks.read().await.get(&id).cloned()
    }

    async fn submit(self: &Arc<Self>, mut request: SubmitTask) -> Result<RuntimeTask> {
        if request.prompt.trim().is_empty() {
            bail!("task prompt must not be empty");
        }
        request.workspace = request
            .workspace
            .canonicalize()
            .with_context(|| format!("invalid workspace: {}", request.workspace.display()))?;
        if let Some(config) = request.config.as_mut() {
            *config = config
                .canonicalize()
                .with_context(|| format!("invalid config: {}", config.display()))?;
        }

        let id = uuid::Uuid::new_v4();
        let mut task = RuntimeTask {
            id,
            status: RuntimeTaskStatus::Queued,
            workspace: request.workspace.clone(),
            profile: request.profile.clone(),
            pid: None,
            created_at: now(),
            started_at: None,
            completed_at: None,
            exit_code: None,
            error: None,
        };
        self.insert_and_persist(task.clone()).await?;
        self.events.append("task.queued", format!("task_id={id}"))?;

        let mut command = tokio::process::Command::new(&self.executable);
        command
            .arg("--json")
            .arg("--no-tui")
            .arg("--web-input-json")
            .arg("--workspace")
            .arg(&request.workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(profile) = &request.profile {
            command.arg("--profile").arg(profile);
        }
        if let Some(config) = &request.config {
            command.arg("--config").arg(config);
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                self.finish(id, RuntimeTaskStatus::Failed, None, Some(error.to_string()))
                    .await?;
                return Err(error).context("spawn Runtime Harness task");
            }
        };
        let pid = child.id();
        let mut stdin = child.stdin.take().context("open Runtime task stdin")?;
        let input = serde_json::to_vec(&serde_json::json!({
            "prompt": request.prompt,
            "attachments": []
        }))?;
        if let Err(error) = stdin.write_all(&input).await {
            let _ = child.kill().await;
            self.finish(id, RuntimeTaskStatus::Failed, None, Some(error.to_string()))
                .await?;
            return Err(error).context("send prompt to Runtime Harness task");
        }
        drop(stdin);

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        task.status = RuntimeTaskStatus::Running;
        task.pid = pid;
        task.started_at = Some(now());
        self.insert_and_persist(task.clone()).await?;
        self.events.append(
            "task.started",
            format!("task_id={id} pid={}", pid.unwrap_or_default()),
        )?;
        let cancellation = Arc::new(Notify::new());
        self.cancellations
            .lock()
            .map_err(|_| anyhow::anyhow!("Runtime task cancellation lock poisoned"))?
            .insert(id, cancellation.clone());

        let manager = self.clone();
        tokio::spawn(async move {
            let stdout_task = stdout.map(|stream| {
                tokio::spawn(forward_task_lines(
                    stream,
                    manager.events.clone(),
                    id,
                    "task.output",
                ))
            });
            let stderr_task = stderr.map(|stream| {
                tokio::spawn(forward_task_lines(
                    stream,
                    manager.events.clone(),
                    id,
                    "task.stderr",
                ))
            });
            let (status, cancelled) = tokio::select! {
                result = child.wait() => (result, false),
                _ = cancellation.notified() => {
                    let _ = child.kill().await;
                    (child.wait().await, true)
                }
            };
            if let Some(task) = stdout_task {
                let _ = task.await;
            }
            if let Some(task) = stderr_task {
                let _ = task.await;
            }
            if let Ok(mut cancellations) = manager.cancellations.lock() {
                cancellations.remove(&id);
            }
            let (final_status, code, error) = match status {
                Ok(exit) if cancelled => (RuntimeTaskStatus::Cancelled, exit.code(), None),
                Ok(exit) if exit.success() => (RuntimeTaskStatus::Completed, exit.code(), None),
                Ok(exit) => (
                    RuntimeTaskStatus::Failed,
                    exit.code(),
                    Some(format!("Harness exited with {exit}")),
                ),
                Err(error) => (RuntimeTaskStatus::Failed, None, Some(error.to_string())),
            };
            if let Err(error) = manager.finish(id, final_status, code, error).await {
                eprintln!("persist Runtime task {id} completion: {error:#}");
            }
        });
        Ok(task)
    }

    async fn cancel(&self, id: uuid::Uuid) -> Result<Option<RuntimeTask>> {
        let cancellation = self
            .cancellations
            .lock()
            .ok()
            .and_then(|items| items.get(&id).cloned());
        if let Some(cancellation) = cancellation {
            let _persistence = self.persistence.lock().await;
            let task = {
                let mut tasks = self.tasks.write().await;
                let Some(task) = tasks.get_mut(&id) else {
                    return Ok(None);
                };
                task.status = RuntimeTaskStatus::Cancelling;
                let task = task.clone();
                persist_tasks(&self.path, &tasks)?;
                task
            };
            cancellation.notify_one();
            self.events
                .append("task.cancellation_requested", format!("task_id={id}"))?;
            return Ok(Some(task));
        }
        Ok(self.get(id).await)
    }

    async fn cancel_all(&self) {
        let cancellations = self
            .cancellations
            .lock()
            .map(|items| items.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for cancellation in cancellations {
            cancellation.notify_one();
        }
        for _ in 0..50 {
            if self
                .cancellations
                .lock()
                .is_ok_and(|items| items.is_empty())
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn insert_and_persist(&self, task: RuntimeTask) -> Result<()> {
        let _persistence = self.persistence.lock().await;
        let snapshot = {
            let mut tasks = self.tasks.write().await;
            tasks.insert(task.id, task);
            tasks.clone()
        };
        persist_tasks(&self.path, &snapshot)
    }

    async fn finish(
        &self,
        id: uuid::Uuid,
        status: RuntimeTaskStatus,
        exit_code: Option<i32>,
        error: Option<String>,
    ) -> Result<()> {
        let _persistence = self.persistence.lock().await;
        let snapshot = {
            let mut tasks = self.tasks.write().await;
            let task = tasks.get_mut(&id).context("Runtime task disappeared")?;
            task.status = status;
            task.completed_at = Some(now());
            task.exit_code = exit_code;
            task.error = error.clone();
            tasks.clone()
        };
        persist_tasks(&self.path, &snapshot)?;
        self.events.append(
            match status {
                RuntimeTaskStatus::Completed => "task.completed",
                RuntimeTaskStatus::Cancelled => "task.cancelled",
                _ => "task.failed",
            },
            format!(
                "task_id={id} exit_code={} error={}",
                exit_code.map_or_else(|| "none".to_owned(), |code| code.to_string()),
                error.unwrap_or_default()
            ),
        )?;
        Ok(())
    }
}

async fn forward_task_lines<R>(
    stream: R,
    events: Arc<EventLog>,
    task_id: uuid::Uuid,
    kind: &'static str,
) where
    R: tokio::io::AsyncRead + Unpin,
{
    const MAX_EVENT_CHARS: usize = 32_768;
    let mut lines = tokio::io::BufReader::new(stream).lines();
    while let Ok(Some(mut line)) = lines.next_line().await {
        if line.len() > MAX_EVENT_CHARS {
            line.truncate(MAX_EVENT_CHARS);
            line.push('…');
        }
        if events
            .append(kind, format!("task_id={task_id} {line}"))
            .is_err()
        {
            break;
        }
    }
}

fn load_tasks(path: &Path) -> Result<HashMap<uuid::Uuid, RuntimeTask>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let tasks: Vec<RuntimeTask> = serde_json::from_slice(&std::fs::read(path)?)?;
    Ok(tasks.into_iter().map(|task| (task.id, task)).collect())
}

fn persist_tasks(path: &Path, tasks: &HashMap<uuid::Uuid, RuntimeTask>) -> Result<()> {
    let mut tasks = tasks.values().cloned().collect::<Vec<_>>();
    tasks.sort_by_key(|task| task.created_at);
    write_json_atomic(path, &tasks)
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
    lock_path: PathBuf,
    lock_token: String,
}

impl Drop for StateCleanup {
    fn drop(&mut self) {
        if load_state(&self.path).is_ok_and(|state| state.token == self.token) {
            let _ = std::fs::remove_file(&self.path);
        }
        remove_owned_lock(&self.lock_path, &self.lock_token);
    }
}

struct OwnedLockCleanup {
    path: PathBuf,
    token: String,
    armed: bool,
}

impl OwnedLockCleanup {
    fn new(path: PathBuf, token: String) -> Self {
        Self {
            path,
            token,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for OwnedLockCleanup {
    fn drop(&mut self) {
        if self.armed {
            remove_owned_lock(&self.path, &self.token);
        }
    }
}

fn load_state(path: &Path) -> Result<DaemonState> {
    let data = std::fs::read(path)
        .with_context(|| format!("read Runtime Daemon state at {}", path.display()))?;
    serde_json::from_slice(&data).context("parse Runtime Daemon state")
}

fn write_state(path: &Path, state: &DaemonState) -> Result<()> {
    write_json_atomic(path, state)
}

fn write_json_atomic<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4().simple()));
    let data = serde_json::to_vec_pretty(value)?;
    write_private(&temporary, &data)?;
    if cfg!(windows) && path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(&temporary, path)?;
    Ok(())
}

fn acquire_daemon_lock(path: &Path) -> Result<DaemonLock> {
    if let Ok(existing) = load_daemon_lock(path) {
        if now().saturating_sub(existing.created_at) > LOCK_STALE_AFTER_SECONDS {
            remove_owned_lock(path, &existing.token);
        } else {
            bail!("Runtime Daemon lock already exists");
        }
    }
    let lock = DaemonLock {
        token: uuid::Uuid::new_v4().simple().to_string(),
        created_at: now(),
    };
    let data = serde_json::to_vec_pretty(&lock)?;
    write_private(path, &data).context("acquire Runtime Daemon single-instance lock")?;
    Ok(lock)
}

fn load_daemon_lock(path: &Path) -> Result<DaemonLock> {
    serde_json::from_slice(&std::fs::read(path)?).context("parse Runtime Daemon lock")
}

fn remove_owned_lock(path: &Path, token: &str) {
    if load_daemon_lock(path).is_ok_and(|lock| lock.token == token) {
        let _ = std::fs::remove_file(path);
    }
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
        let root =
            std::env::temp_dir().join(format!("willdeep-daemon-auth-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let events = Arc::new(EventLog::open(root.join("events.ndjson")).unwrap());
        let state = ServerState {
            token: "expected".to_owned(),
            started_at: 0,
            shutdown: Arc::new(Notify::new()),
            events: events.clone(),
            tasks: Arc::new(
                TaskManager::open(root.join("tasks.json"), PathBuf::from("willdeep"), events)
                    .unwrap(),
            ),
        };
        assert_eq!(
            authorize(&state, &HeaderMap::new()),
            Err(StatusCode::UNAUTHORIZED)
        );
        let mut headers = HeaderMap::new();
        headers.insert(TOKEN_HEADER, HeaderValue::from_static("expected"));
        assert_eq!(authorize(&state, &headers), Ok(()));
        std::fs::remove_dir_all(root).unwrap();
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

    #[test]
    fn task_store_marks_active_tasks_interrupted_after_restart() {
        let root = std::env::temp_dir().join(format!("willdeep-tasks-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("tasks.json");
        let id = uuid::Uuid::new_v4();
        let task = RuntimeTask {
            id,
            status: RuntimeTaskStatus::Running,
            workspace: root.clone(),
            profile: None,
            pid: Some(10),
            created_at: 1,
            started_at: Some(2),
            completed_at: None,
            exit_code: None,
            error: None,
        };
        persist_tasks(&path, &HashMap::from([(id, task)])).unwrap();
        let events = Arc::new(EventLog::open(root.join("events.ndjson")).unwrap());
        let manager = TaskManager::open(path, PathBuf::from("willdeep"), events).unwrap();
        let recovered = manager.tasks.blocking_read();
        assert_eq!(recovered[&id].status, RuntimeTaskStatus::Interrupted);
        assert!(recovered[&id].completed_at.is_some());
        drop(recovered);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn daemon_lock_is_exclusive_and_owned_cleanup_is_safe() {
        let root = std::env::temp_dir().join(format!("willdeep-lock-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("daemon.lock");
        let first = acquire_daemon_lock(&path).unwrap();
        assert!(acquire_daemon_lock(&path).is_err());
        remove_owned_lock(&path, "not-the-owner");
        assert!(path.exists());
        remove_owned_lock(&path, &first.token);
        assert!(acquire_daemon_lock(&path).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn daemon_lock_recovers_after_stale_lease() {
        let root =
            std::env::temp_dir().join(format!("willdeep-stale-lock-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("daemon.lock");
        write_json_atomic(
            &path,
            &DaemonLock {
                token: "stale".to_owned(),
                created_at: 0,
            },
        )
        .unwrap();
        assert_ne!(acquire_daemon_lock(&path).unwrap().token, "stale");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn concurrent_task_updates_persist_a_complete_snapshot() {
        let root = std::env::temp_dir().join(format!(
            "willdeep-concurrent-tasks-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("tasks.json");
        let events = Arc::new(EventLog::open(root.join("events.ndjson")).unwrap());
        let manager =
            Arc::new(TaskManager::open(path.clone(), PathBuf::from("willdeep"), events).unwrap());
        let mut updates = Vec::new();
        for index in 0..20 {
            let manager = manager.clone();
            let workspace = root.clone();
            updates.push(tokio::spawn(async move {
                let id = uuid::Uuid::new_v4();
                manager
                    .insert_and_persist(RuntimeTask {
                        id,
                        status: RuntimeTaskStatus::Completed,
                        workspace,
                        profile: None,
                        pid: None,
                        created_at: index,
                        started_at: Some(index),
                        completed_at: Some(index),
                        exit_code: Some(0),
                        error: None,
                    })
                    .await
                    .unwrap();
            }));
        }
        for update in updates {
            update.await.unwrap();
        }
        assert_eq!(load_tasks(&path).unwrap().len(), 20);
        std::fs::remove_dir_all(root).unwrap();
    }
}
