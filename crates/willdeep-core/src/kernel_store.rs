//! 事件日志的落盘与恢复。
//!
//! 每个会话一个文件：`<home>/agent-events/<session-id>.json`，0600 权限，
//! 原子替换。跨进程重启之后，没投递完的事件继续投递；投递到一半断电的
//! lease 回到待投递，而不是停在「已经交给模型了」。
//!
//! # 三条落地纪律
//!
//! - **淘汰之后每个受影响的会话各自重写自己的文件。** 只写当前这个会话的
//!   话，被全局上限淘汰掉的事件会在重启后从别人的文件里回来。
//! - **解不出来的文件一律隔离，不删。** 解不出来意味着未知，未知就留着：
//!   `.corrupt-<时间戳>` 改名保留现场，启动继续，不阻塞。
//! - **加载失败不是启动失败。** 事件是通知，不是账本；读不出来最坏是漏一
//!   条提醒，为它挡住整个 Runtime 起不来才是真的坏。

use std::path::{Path, PathBuf};

use uuid::Uuid;
use willdeep_runtime_protocol::kernel_event::{
    KERNEL_EVENT_SCHEMA_VERSION, KernelEvent, MAX_EVENTS_PER_SESSION,
};

const DIRECTORY: &str = "agent-events";

/// 磁盘上的一份会话事件日志。
#[derive(serde::Serialize, serde::Deserialize)]
struct EventLogFile {
    schema_version: String,
    events: Vec<KernelEvent>,
}

#[derive(Clone, Debug)]
pub struct KernelStore {
    directory: PathBuf,
}

/// 一次加载的结果。**坏文件不是错误**，是一条要说出来的事实：调用方需要知道
/// 「这个会话的历史事件被隔离了」，才能解释为什么提醒少了一条。
#[derive(Debug, Default)]
pub struct LoadReport {
    pub sessions: Vec<(Uuid, Vec<KernelEvent>)>,
    /// 被隔离的文件与原因。
    pub quarantined: Vec<(PathBuf, String)>,
}

impl LoadReport {
    pub fn events(&self) -> usize {
        self.sessions.iter().map(|(_, events)| events.len()).sum()
    }
}

impl KernelStore {
    pub fn new(home: impl AsRef<Path>) -> Self {
        Self {
            directory: home.as_ref().join(DIRECTORY),
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// 读回全部会话的事件。
    ///
    /// 目录不存在是正常状态（第一次运行），不是错误。
    pub fn load_all(&self) -> LoadReport {
        let mut report = LoadReport::default();
        let Ok(entries) = std::fs::read_dir(&self.directory) else {
            return report;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|value| value != "json") {
                continue;
            }
            let Some(id) = path
                .file_stem()
                .and_then(|value| value.to_str())
                .and_then(|value| Uuid::parse_str(value).ok())
            else {
                continue;
            };
            match self.read(&path) {
                Ok(events) => report.sessions.push((id, events)),
                Err(reason) => {
                    let quarantined = self.quarantine(&path);
                    report.quarantined.push((quarantined, reason));
                }
            }
        }
        report
    }

    pub fn load_session(&self, session_id: Uuid) -> Result<Vec<KernelEvent>, String> {
        let path = self.path(session_id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        self.read(&path)
    }

    /// 写下一个会话的全部事件。空数组等于删除文件——留一个空壳只会让下次
    /// 加载多读一次。
    pub fn save_session(&self, session_id: Uuid, events: &[KernelEvent]) -> std::io::Result<()> {
        if events.is_empty() {
            return self.delete_session(session_id).map(|_| ());
        }
        std::fs::create_dir_all(&self.directory)?;
        let file = EventLogFile {
            schema_version: KERNEL_EVENT_SCHEMA_VERSION.to_owned(),
            // 只留每会话上限内的最新那些。上限在契约里，两端同一个数。
            events: events
                .iter()
                .skip(events.len().saturating_sub(MAX_EVENTS_PER_SESSION))
                .cloned()
                .collect(),
        };
        let data = serde_json::to_vec_pretty(&file)
            .map_err(|error| std::io::Error::other(format!("encode event log: {error}")))?;
        let temporary = self
            .directory
            .join(format!(".{session_id}.{}.tmp", Uuid::new_v4()));
        write_private(&temporary, &data)?;
        std::fs::rename(&temporary, self.path(session_id))
    }

    /// 会话归档或删除时连它的事件一起清掉。
    pub fn delete_session(&self, session_id: Uuid) -> std::io::Result<bool> {
        let path = self.path(session_id);
        if !path.exists() {
            return Ok(false);
        }
        std::fs::remove_file(path)?;
        Ok(true)
    }

    fn read(&self, path: &Path) -> Result<Vec<KernelEvent>, String> {
        let bytes =
            std::fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
        let file: EventLogFile = serde_json::from_slice(&bytes)
            .map_err(|error| format!("decode {}: {error}", path.display()))?;
        if file.schema_version != KERNEL_EVENT_SCHEMA_VERSION {
            return Err(format!(
                "unsupported event log schema {} in {}",
                file.schema_version,
                path.display()
            ));
        }
        // 单条事件坏掉不牵连整份日志：能读懂的照常恢复，读不懂的丢掉。整份
        // 文件解不出来才走隔离。
        Ok(file
            .events
            .into_iter()
            .filter(|event| event.validate().is_ok())
            .collect())
    }

    fn quarantine(&self, path: &Path) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_secs())
            .unwrap_or_default();
        let target = path.with_extension(format!("corrupt-{stamp}"));
        let _ = std::fs::rename(path, &target);
        target
    }

    fn path(&self, session_id: Uuid) -> PathBuf {
        self.directory.join(format!("{session_id}.json"))
    }
}

/// 把内核里变过的会话写下去。
///
/// 返回写了几个会话。**取脏与写盘之间没有锁**：这中间新到的事件会把会话重新
/// 标脏，下一次刷盘带上，不会丢——丢的前提是「标脏之后不写」，而不是「写完
/// 之后又变了」。
pub fn flush(kernel: &crate::kernel::EventKernel, store: &KernelStore) -> Vec<(Uuid, String)> {
    let mut failures = Vec::new();
    for session in kernel.take_dirty_sessions() {
        let events = kernel.session_events(session);
        if let Err(error) = store.save_session(session, &events) {
            failures.push((session, error.to_string()));
        }
    }
    failures
}

/// 启动时把磁盘上的事件装回内核。
///
/// 加载失败不阻塞启动：事件是通知不是账本，读不出来最坏漏一条提醒，为它挡住
/// 整个 Runtime 起不来才是真的坏。被隔离的文件在返回的报告里，调用方决定要不
/// 要说给用户听。
pub fn restore_into(kernel: &crate::kernel::EventKernel, store: &KernelStore) -> LoadReport {
    let report = store.load_all();
    for (_, events) in &report.sessions {
        kernel.restore(events.clone());
    }
    // 刚装进去的这些就是磁盘上的原样，没必要马上重写一遍。
    let _ = kernel.take_dirty_sessions();
    report
}

/// 事件正文可能带着外部消息与工具输出，与会话转录同级敏感，所以文件是 0600。
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::{DedupPolicy, EventKernel, InterruptPolicy, host_event};
    use willdeep_runtime_protocol::kernel_event::{DeliveryState, EventPriority, EventSource};

    fn store() -> (KernelStore, PathBuf) {
        let home = std::env::temp_dir().join(format!("willdeep-events-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&home).expect("home");
        (KernelStore::new(&home), home)
    }

    fn event(session: Uuid, key: &str) -> KernelEvent {
        host_event(
            session,
            EventSource::Worker,
            "worker.completed",
            EventPriority::Normal,
            InterruptPolicy::YieldAtBoundary,
            key,
            None,
            Some(key.to_owned()),
            false,
        )
    }

    #[test]
    fn events_survive_a_restart_and_leases_come_back_pending() {
        let (store, _home) = store();
        let session = Uuid::new_v4();
        let kernel = EventKernel::new();
        kernel.publish(event(session, "a"), DedupPolicy::Once);
        kernel.publish(event(session, "b"), DedupPolicy::Once);
        // 一条已经交给模型，然后「断电」。
        let leased = kernel.take_for_model(1);
        assert_eq!(leased.len(), 1);
        store
            .save_session(session, &kernel.snapshot())
            .expect("save");

        let restored = EventKernel::new();
        let report = store.load_all();
        assert_eq!(report.sessions.len(), 1);
        restored.restore(report.sessions[0].1.clone());
        assert_eq!(restored.len(), 2);
        assert!(
            restored
                .snapshot()
                .iter()
                .all(|stored| stored.delivery.state == DeliveryState::Pending),
            "投递到一半断电不等于已经处理"
        );
        // 恢复后重投时，去重键要认得出这些事件已经收过了。
        assert!(
            !restored
                .publish(event(session, "a"), DedupPolicy::Once)
                .is_new()
        );
    }

    /// 解不出来的文件被隔离而不是被删，启动继续。
    #[test]
    fn a_corrupt_log_is_quarantined_not_deleted() {
        let (store, _home) = store();
        let session = Uuid::new_v4();
        store
            .save_session(session, &[event(session, "a")])
            .expect("save");
        std::fs::write(
            store.directory().join(format!("{session}.json")),
            b"{ not json",
        )
        .expect("corrupt it");

        let report = store.load_all();
        assert!(report.sessions.is_empty());
        assert_eq!(report.quarantined.len(), 1);
        let (path, _) = &report.quarantined[0];
        assert!(path.exists(), "现场必须留着，未知不等于可以删");
        assert!(
            path.to_string_lossy().contains("corrupt-"),
            "隔离文件名要一眼看得出是什么"
        );
        // 隔离之后目录照常可用。
        store
            .save_session(session, &[event(session, "b")])
            .expect("save again");
        assert_eq!(store.load_all().sessions.len(), 1);
    }

    /// 一条坏事件不牵连整份日志。
    #[test]
    fn one_bad_event_does_not_take_the_log_down() {
        let (store, _home) = store();
        let session = Uuid::new_v4();
        let mut broken = event(session, "bad");
        broken.title = String::new();
        store
            .save_session(session, &[event(session, "good"), broken])
            .expect("save");
        let events = store.load_session(session).expect("load");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].title, "good");
    }

    #[cfg(unix)]
    #[test]
    fn logs_are_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt;
        let (store, _home) = store();
        let session = Uuid::new_v4();
        store
            .save_session(session, &[event(session, "a")])
            .expect("save");
        let mode = std::fs::metadata(store.directory().join(format!("{session}.json")))
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "事件正文与会话转录同级敏感");
    }

    #[test]
    fn deleting_a_session_takes_its_log() {
        let (store, _home) = store();
        let session = Uuid::new_v4();
        store
            .save_session(session, &[event(session, "a")])
            .expect("save");
        assert!(store.delete_session(session).expect("delete"));
        assert!(store.load_all().sessions.is_empty());
        // 再删一次不报错：清理路径要幂等。
        assert!(!store.delete_session(session).expect("delete again"));
    }

    /// 全局淘汰牵连到的会话必须各自重写文件，否则「删掉的事件」会从别人的
    /// 文件里诈尸回来。
    #[test]
    fn a_globally_evicted_event_does_not_come_back_from_another_log() {
        use willdeep_runtime_protocol::kernel_event::MAX_EVENTS_GLOBAL;
        let (store, _home) = store();
        let kernel = EventKernel::new();
        let victim_session = Uuid::new_v4();
        // 先给受害会话写一条并落盘：它此刻在磁盘上有文件。
        kernel.publish(event(victim_session, "victim"), DedupPolicy::Once);
        flush(&kernel, &store);
        assert_eq!(store.load_session(victim_session).expect("load").len(), 1);

        // 再用别的会话把全局上限撑爆，逼出全局淘汰。
        for index in 0..MAX_EVENTS_GLOBAL + 5 {
            kernel.publish(
                event(Uuid::new_v4(), &format!("f{index}")),
                DedupPolicy::Once,
            );
        }
        flush(&kernel, &store);

        assert!(
            kernel
                .snapshot()
                .iter()
                .all(|stored| stored.session_id != victim_session),
            "这条事件应该已经被全局淘汰"
        );
        assert!(
            store.load_session(victim_session).expect("load").is_empty(),
            "内存里淘汰了，磁盘上也必须跟着没"
        );
    }

    /// 归档或删除会话之后，它的日志不该在下次启动时把正文带回来。
    #[test]
    fn forgetting_a_session_removes_its_log_on_the_next_flush() {
        let (store, _home) = store();
        let kernel = EventKernel::new();
        let session = Uuid::new_v4();
        kernel.publish(event(session, "a"), DedupPolicy::Once);
        flush(&kernel, &store);
        assert!(!store.load_session(session).expect("load").is_empty());

        kernel.forget_session(session);
        flush(&kernel, &store);
        assert!(store.load_session(session).expect("load").is_empty());
    }

    /// 恢复之后不该立刻把刚读进来的内容原样再写一遍。
    #[test]
    fn restoring_does_not_immediately_dirty_everything() {
        let (store, _home) = store();
        let kernel = EventKernel::new();
        let session = Uuid::new_v4();
        kernel.publish(event(session, "a"), DedupPolicy::Once);
        flush(&kernel, &store);

        let restored = EventKernel::new();
        let report = restore_into(&restored, &store);
        assert_eq!(report.events(), 1);
        assert!(restored.take_dirty_sessions().is_empty());
    }

    /// 保存时按每会话上限截断，留最新的那些。
    #[test]
    fn saving_respects_the_per_session_ceiling() {
        let (store, _home) = store();
        let session = Uuid::new_v4();
        let events: Vec<KernelEvent> = (0..MAX_EVENTS_PER_SESSION + 30)
            .map(|index| event(session, &format!("k{index}")))
            .collect();
        store.save_session(session, &events).expect("save");
        let loaded = store.load_session(session).expect("load");
        assert_eq!(loaded.len(), MAX_EVENTS_PER_SESSION);
        assert_eq!(
            loaded.last().expect("last").title,
            format!("k{}", MAX_EVENTS_PER_SESSION + 29),
            "截断要留最新的，不是最早的"
        );
    }
}
