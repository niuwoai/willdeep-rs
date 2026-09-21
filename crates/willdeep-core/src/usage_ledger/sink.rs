//! 账本的唯一写入口：有界通道 + 单个后台写线程。
//!
//! 热路径上只做一次 `try_send`；满了就丢这一行并计数，绝不阻塞回合。写线程
//! 每收到一行就以 `O_APPEND|O_CREAT`（Unix 下权限 0600）打开当月文件、一次
//! `write` 写完整行。打开 / 写失败只警告，不重试、不上抛（规范 §7 不变量 4）。

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use super::UsageLedgerRecord;

/// 在途上限。账本写入快，正常情况下通道几乎总是空的；满了说明磁盘卡死，
/// 宁可丢账也不能让回合等。
const CHANNEL_CAPACITY: usize = 1_024;

/// 进程退出前等写线程排空的上限。
pub const EXIT_FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

enum Command {
    Write(Box<UsageLedgerRecord>),
    Flush(SyncSender<()>),
}

/// 一个账本目录的写入器。可以直接 [`UsageLedgerSink::spawn`] 一个独立实例
/// （测试这样用），生产路径走按目录共享的 [`shared_sink`]。
pub struct UsageLedgerSink {
    dir: PathBuf,
    sender: Mutex<Option<SyncSender<Command>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    stats: Arc<SinkStats>,
}

#[derive(Default)]
struct SinkStats {
    written: AtomicU64,
    failed: AtomicU64,
    dropped: AtomicU64,
    warned: AtomicBool,
}

impl SinkStats {
    /// 每个写入器只喊一次：TUI 占着终端，反复往 stderr 打字会把界面刷花。
    fn warn(&self, dir: &Path, message: &str) {
        if !self.warned.swap(true, Ordering::Relaxed) {
            eprintln!(
                "willdeep: usage ledger at {} is not recording ({message}); the turn is unaffected and further ledger errors are suppressed",
                dir.display()
            );
        }
    }
}

impl UsageLedgerSink {
    pub fn spawn(dir: impl Into<PathBuf>) -> Arc<Self> {
        let dir = dir.into();
        let stats = Arc::new(SinkStats::default());
        let (sender, receiver) = sync_channel(CHANNEL_CAPACITY);
        let worker = {
            let dir = dir.clone();
            let stats = stats.clone();
            std::thread::Builder::new()
                .name("willdeep-usage-ledger".to_owned())
                .spawn(move || run_writer(&dir, receiver, &stats))
        };
        let worker = match worker {
            Ok(worker) => Some(worker),
            Err(error) => {
                stats.warn(&dir, &format!("cannot start writer thread: {error}"));
                None
            }
        };
        Arc::new(Self {
            sender: Mutex::new(worker.is_some().then_some(sender)),
            worker: Mutex::new(worker),
            dir,
            stats,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 交给写线程。永不阻塞、永不失败——丢了也只计数。
    pub fn submit(&self, record: UsageLedgerRecord) {
        let Ok(sender) = self.sender.lock() else {
            return;
        };
        let Some(sender) = sender.as_ref() else {
            self.stats.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        match sender.try_send(Command::Write(Box::new(record))) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                self.stats.warn(&self.dir, "writer queue is full");
            }
            Err(TrySendError::Disconnected(_)) => {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// 等写线程把此前交来的记录全部落盘；超时返回 `false`。
    pub fn flush(&self, timeout: Duration) -> bool {
        let (ack, done) = sync_channel(1);
        let sent = self
            .sender
            .lock()
            .ok()
            .and_then(|sender| sender.as_ref().map(|sender| sender.clone()))
            .is_some_and(|sender| sender.send(Command::Flush(ack)).is_ok());
        sent && done.recv_timeout(timeout).is_ok()
    }

    /// 已成功写入的行数。
    pub fn written(&self) -> u64 {
        self.stats.written.load(Ordering::Relaxed)
    }

    /// 写入失败（打开或写文件出错）的行数。
    pub fn failed(&self) -> u64 {
        self.stats.failed.load(Ordering::Relaxed)
    }

    /// 因队列满或写线程不在而丢弃的行数。
    pub fn dropped(&self) -> u64 {
        self.stats.dropped.load(Ordering::Relaxed)
    }
}

impl Drop for UsageLedgerSink {
    fn drop(&mut self) {
        // 先断开发送端，写线程排空剩余记录后自然退出，再等它收尾。
        if let Ok(mut sender) = self.sender.lock() {
            sender.take();
        }
        if let Some(worker) = self.worker.lock().ok().and_then(|mut worker| worker.take()) {
            let _ = worker.join();
        }
    }
}

fn run_writer(dir: &Path, receiver: Receiver<Command>, stats: &SinkStats) {
    while let Ok(command) = receiver.recv() {
        match command {
            Command::Write(record) => match append_record(dir, &record) {
                Ok(()) => {
                    stats.written.fetch_add(1, Ordering::Relaxed);
                }
                Err(error) => {
                    stats.failed.fetch_add(1, Ordering::Relaxed);
                    stats.warn(dir, &error);
                }
            },
            Command::Flush(ack) => {
                let _ = ack.send(());
            }
        }
    }
}

/// 把一条记录追加进它所属月份的文件。公开给回填命令：回填与实时写入走同一
/// 条落盘路径，行格式与原子性保证一致。
pub fn append_record(dir: &Path, record: &UsageLedgerRecord) -> Result<(), String> {
    let line = record
        .to_line()
        .ok_or_else(|| "record does not fit in one ledger line".to_owned())?;
    let path = dir.join(record.month_file_name());
    let mut file = match open_append(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(dir)
                .map_err(|error| format!("create {}: {error}", dir.display()))?;
            open_append(&path).map_err(|error| format!("open {}: {error}", path.display()))?
        }
        Err(error) => return Err(format!("open {}: {error}", path.display())),
    };
    // 一次 write 写完整行：O_APPEND 下小于 PIPE_BUF 的写入在本地文件系统上是
    // 原子的。万一只写了一部分（理论上不会），补写剩余部分，宁可这一行与别
    // 人交错被读端当坏行跳过，也不留半行卡住后面的读取。
    let written = file
        .write(&line)
        .map_err(|error| format!("write {}: {error}", path.display()))?;
    if written < line.len() {
        file.write_all(&line[written..])
            .map_err(|error| format!("write {}: {error}", path.display()))?;
    }
    Ok(())
}

fn open_append(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn registry() -> &'static Mutex<HashMap<PathBuf, Arc<UsageLedgerSink>>> {
    static SINKS: OnceLock<Mutex<HashMap<PathBuf, Arc<UsageLedgerSink>>>> = OnceLock::new();
    SINKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 进程内按账本目录共享的写入器：同一个 `$WILLDEEP_HOME` 只起一个写线程，
/// daemon 里每个任务、TUI 里每次重建 harness 都复用它。
pub fn shared_sink(dir: &Path) -> Arc<UsageLedgerSink> {
    let Ok(mut sinks) = registry().lock() else {
        return UsageLedgerSink::spawn(dir);
    };
    sinks
        .entry(dir.to_path_buf())
        .or_insert_with(|| UsageLedgerSink::spawn(dir))
        .clone()
}

/// 进程正常退出前调用：等所有共享写入器排空。每个最多等 `timeout`。
pub fn flush_all(timeout: Duration) {
    let sinks = registry()
        .lock()
        .map(|sinks| sinks.values().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    for sink in sinks {
        sink.flush(timeout);
    }
}
