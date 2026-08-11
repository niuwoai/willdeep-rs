use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::Message;
use crate::types::Role;

pub const SESSION_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompressionCheckpoint {
    pub generation: u64,
    pub previous_message_count: usize,
    pub compressed_message_count: usize,
    pub created_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub version: u32,
    pub id: Uuid,
    pub title: String,
    pub workspace: PathBuf,
    pub profile: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub config: Option<PathBuf>,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default)]
    pub pinned_at: Option<u64>,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub attention_read: BTreeSet<String>,
    #[serde(default)]
    pub runtime_event_cursor: u64,
    #[serde(default)]
    pub runtime_managed: bool,
    #[serde(default)]
    pub goal: Option<String>,
    #[serde(default)]
    pub compression_generation: u64,
    #[serde(default)]
    pub compression_checkpoint: Option<CompressionCheckpoint>,
    #[serde(skip)]
    pub swift_source: Option<PathBuf>,
}

impl Session {
    pub fn new(workspace: PathBuf, profile: Option<String>, prompt: &str) -> Self {
        let now = now();
        Self {
            version: SESSION_VERSION,
            id: Uuid::new_v4(),
            title: title(prompt),
            workspace,
            profile,
            model: None,
            config: None,
            created_at: now,
            updated_at: now,
            pinned_at: None,
            messages: Vec::new(),
            attention_read: BTreeSet::new(),
            runtime_event_cursor: 0,
            runtime_managed: false,
            goal: None,
            compression_generation: 0,
            compression_checkpoint: None,
            swift_source: None,
        }
    }

    pub fn replace_with_compressed_messages(&mut self, messages: Vec<Message>) -> bool {
        let previous_message_count = self.messages.len();
        let compressed_message_count = messages.len();
        self.messages = messages;
        if compressed_message_count >= previous_message_count {
            return false;
        }
        self.compression_generation = self.compression_generation.saturating_add(1);
        self.compression_checkpoint = Some(CompressionCheckpoint {
            generation: self.compression_generation,
            previous_message_count,
            compressed_message_count,
            created_at: now(),
        });
        true
    }
}

#[derive(Clone, Debug)]
pub struct SessionStore {
    directory: PathBuf,
}

impl SessionStore {
    pub fn new(home: impl AsRef<Path>) -> Self {
        Self {
            directory: home.as_ref().join("sessions"),
        }
    }
    pub fn load(&self, id: Uuid) -> Result<Session, SessionError> {
        let local = self.path(id);
        let session: Session = if local.exists() {
            serde_json::from_slice(&std::fs::read(local)?)?
        } else if let Some(path) = swift_session_directory()
            .map(|dir| dir.join(format!("{id}.json")))
            .filter(|path| path.exists())
        {
            swift_session(&path)?
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("session {id} not found"),
            )
            .into());
        };
        if session.version != SESSION_VERSION {
            return Err(SessionError::Version(session.version));
        }
        Ok(session)
    }
    pub fn latest(&self) -> Result<Option<Session>, SessionError> {
        Ok(self.list()?.into_iter().max_by_key(|s| s.updated_at))
    }
    pub fn list(&self) -> Result<Vec<Session>, SessionError> {
        let mut values = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.directory) {
            for entry in entries.flatten() {
                if entry.path().extension().and_then(|v| v.to_str()) != Some("json") {
                    continue;
                }
                if let Ok(data) = std::fs::read(entry.path())
                    && let Ok(session) = serde_json::from_slice::<Session>(&data)
                {
                    values.push(session);
                }
            }
        }
        if let Some(directory) = swift_session_directory()
            && let Ok(entries) = std::fs::read_dir(directory)
        {
            for entry in entries.flatten() {
                if entry.path().extension().and_then(|value| value.to_str()) == Some("json")
                    && let Ok(session) = swift_session(&entry.path())
                    && !values.iter().any(|existing| existing.id == session.id)
                {
                    values.push(session);
                }
            }
        }
        values.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        Ok(values)
    }
    pub fn save(&self, session: &mut Session) -> Result<(), SessionError> {
        session.updated_at = now();
        self.write(session)
    }
    /// 置顶/取消置顶。不改动 `updated_at`，避免打乱最近使用排序；
    /// 对 Xedit 桥接会话就地补丁其 JSON 的 `pinnedAt`（ISO8601），
    /// 不在本地生成会覆盖 Xedit 实时内容的影子副本。
    pub fn set_pinned(&self, id: Uuid, pinned: bool) -> Result<Session, SessionError> {
        let mut session = self.load(id)?;
        session.pinned_at = if pinned { Some(now()) } else { None };
        if let Some(source) = session.swift_source.clone() {
            let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&source)?)?;
            let object = value.as_object_mut().ok_or_else(|| {
                serde_json::Error::io(std::io::Error::other("invalid Swift session object"))
            })?;
            match session.pinned_at {
                Some(at) => {
                    object.insert(
                        "pinnedAt".to_owned(),
                        serde_json::Value::String(format_iso8601(at)),
                    );
                }
                None => {
                    object.remove("pinnedAt");
                }
            }
            let temporary = source.with_extension(format!("{}.tmp", Uuid::new_v4()));
            std::fs::write(&temporary, serde_json::to_vec_pretty(&value)?)?;
            std::fs::rename(&temporary, &source)?;
        } else {
            self.write(&session)?;
        }
        Ok(session)
    }
    fn write(&self, session: &Session) -> Result<(), SessionError> {
        std::fs::create_dir_all(&self.directory)?;
        let data = serde_json::to_vec_pretty(session)?;
        let temporary = self
            .directory
            .join(format!(".{}.{}.tmp", session.id, Uuid::new_v4()));
        std::fs::write(&temporary, data)?;
        std::fs::rename(&temporary, self.path(session.id))?;
        Ok(())
    }
    pub fn delete(&self, id: Uuid) -> Result<bool, SessionError> {
        let path = self.path(id);
        if !path.exists() {
            return Ok(false);
        }
        std::fs::remove_file(path)?;
        Ok(true)
    }
    fn path(&self, id: Uuid) -> PathBuf {
        self.directory.join(format!("{id}.json"))
    }
}

fn swift_session_directory() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Some(
            PathBuf::from(std::env::var_os("HOME")?)
                .join("Library/Application Support/WillDeep/agent-sessions"),
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

fn swift_session(path: &Path) -> Result<Session, SessionError> {
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let id = value
        .get("id")
        .and_then(|value| value.as_str())
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or_else(|| serde_json::Error::io(std::io::Error::other("invalid Swift session id")))?;
    let workspace = value
        .get("workspaceLocation")
        .and_then(|value| value.get("path"))
        .and_then(|value| value.as_str())
        .or_else(|| {
            value
                .get("workspaceRootPath")
                .and_then(|value| value.as_str())
        })
        .unwrap_or(".");
    let messages = value
        .get("messages")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|message| {
            let role = match message.get("role")?.as_str()? {
                "user" => Role::User,
                "assistant" => Role::Assistant,
                "system" => Role::System,
                "tool" => Role::Tool,
                _ => return None,
            };
            Some(Message {
                role,
                content: message
                    .get("content")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_owned(),
                tool_call_id: None,
                tool_calls: Vec::new(),
                attachments: Vec::new(),
            })
        })
        .collect();
    let updated = std::fs::metadata(path)?
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_secs())
        .unwrap_or_default();
    Ok(Session {
        version: SESSION_VERSION,
        id,
        title: value
            .get("title")
            .and_then(|value| value.as_str())
            .unwrap_or("Swift session")
            .to_owned(),
        workspace: PathBuf::from(workspace),
        profile: None,
        model: None,
        config: None,
        created_at: updated,
        updated_at: updated,
        pinned_at: value
            .get("pinnedAt")
            .and_then(|value| value.as_str())
            .and_then(parse_iso8601),
        messages,
        attention_read: BTreeSet::new(),
        runtime_event_cursor: 0,
        runtime_managed: false,
        goal: None,
        compression_generation: 0,
        compression_checkpoint: None,
        swift_source: Some(path.to_path_buf()),
    })
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("unsupported session version {0}")]
    Version(u32),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// 解析 Xedit（Swift `JSONEncoder.dateEncodingStrategy = .iso8601`）写出的
/// UTC 时间戳，如 `2026-08-11T12:34:56Z`；容忍小数秒与 `+00:00` 形式。
fn parse_iso8601(text: &str) -> Option<u64> {
    let text = text.trim();
    let (date, time) = text.split_once('T')?;
    let mut parts = date.splitn(3, '-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let time = time
        .trim_end_matches('Z')
        .trim_end_matches("+00:00")
        .trim_end_matches("+0000");
    let time = time.split_once('.').map(|(v, _)| v).unwrap_or(time);
    let mut parts = time.splitn(3, ':');
    let hour: u64 = parts.next()?.parse().ok()?;
    let minute: u64 = parts.next()?.parse().ok()?;
    let second: u64 = parts.next().unwrap_or("0").parse().ok()?;
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    if days < 0 {
        return None;
    }
    Some(days as u64 * 86_400 + hour * 3_600 + minute * 60 + second)
}

fn format_iso8601(timestamp: u64) -> String {
    let days = (timestamp / 86_400) as i64;
    let seconds = timestamp % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds / 3_600,
        seconds % 3_600 / 60,
        seconds % 60
    )
}

// Howard Hinnant 的 days_from_civil / civil_from_days 算法（公历、以 1970-01-01 为第 0 天）。
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = (year - era * 400) as u64;
    let doy =
        (153 * (if month > 2 { month - 3 } else { month + 9 }) as u64 + 2) / 5 + day as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let doe = (days - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}
fn title(prompt: &str) -> String {
    prompt
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(80)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn atomically_round_trips_session() {
        let root = std::env::temp_dir().join(format!("willdeep-session-{}", Uuid::new_v4()));
        let store = SessionStore::new(&root);
        let mut session = Session::new(root.clone(), None, "hello session");
        session.config = Some(root.join("config.toml"));
        session.model = Some("test-model".to_owned());
        session.goal = Some("finish the migration".to_owned());
        session.messages.push(Message::user_with_attachments(
            "hello",
            vec![crate::MessageAttachment::Image {
                name: "test.png".to_owned(),
                media_type: "image/png".to_owned(),
                data: "YWJj".to_owned(),
                width: 1,
                height: 1,
            }],
        ));
        store.save(&mut session).unwrap();
        let loaded = store.load(session.id).unwrap();
        assert_eq!(loaded.config, session.config);
        assert_eq!(loaded.model, session.model);
        assert_eq!(loaded.goal, session.goal);
        assert_eq!(loaded.compression_generation, 0);
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(loaded.messages[0].attachments.len(), 1);
        assert!(loaded.attention_read.is_empty());
        session.attention_read.insert("job_123".to_owned());
        store.save(&mut session).unwrap();
        assert!(
            store
                .load(session.id)
                .unwrap()
                .attention_read
                .contains("job_123")
        );
        assert!(store.delete(session.id).unwrap());
        assert!(!store.delete(session.id).unwrap());
        assert!(store.load(session.id).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pins_session_without_touching_updated_at() {
        let root = std::env::temp_dir().join(format!("willdeep-session-pin-{}", Uuid::new_v4()));
        let store = SessionStore::new(&root);
        let mut session = Session::new(root.clone(), None, "pin me");
        store.save(&mut session).unwrap();
        let saved_updated_at = store.load(session.id).unwrap().updated_at;
        let pinned = store.set_pinned(session.id, true).unwrap();
        assert!(pinned.pinned_at.is_some());
        let loaded = store.load(session.id).unwrap();
        assert_eq!(loaded.pinned_at, pinned.pinned_at);
        assert_eq!(loaded.updated_at, saved_updated_at);
        let unpinned = store.set_pinned(session.id, false).unwrap();
        assert_eq!(unpinned.pinned_at, None);
        assert_eq!(store.load(session.id).unwrap().pinned_at, None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parses_and_formats_iso8601_round_trip() {
        assert_eq!(parse_iso8601("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso8601("2026-08-11T00:00:00Z"), Some(1_786_406_400));
        assert_eq!(
            parse_iso8601("2026-08-11T12:34:56.789Z"),
            Some(1_786_451_696)
        );
        assert_eq!(
            parse_iso8601("2026-08-11T12:34:56+00:00"),
            parse_iso8601("2026-08-11T12:34:56Z")
        );
        assert_eq!(parse_iso8601("not a date"), None);
        for timestamp in [0, 951_827_696, 1_786_451_696, 4_102_444_799] {
            assert_eq!(parse_iso8601(&format_iso8601(timestamp)), Some(timestamp));
        }
        assert_eq!(format_iso8601(1_786_451_696), "2026-08-11T12:34:56Z");
    }

    #[test]
    fn legacy_session_without_config_remains_readable() {
        let session = Session::new(PathBuf::from("/workspace"), None, "legacy");
        let mut value = serde_json::to_value(session).unwrap();
        value.as_object_mut().unwrap().remove("config");
        value
            .as_object_mut()
            .unwrap()
            .remove("compression_generation");
        value
            .as_object_mut()
            .unwrap()
            .remove("compression_checkpoint");
        let loaded: Session = serde_json::from_value(value).unwrap();
        assert_eq!(loaded.config, None);
        assert_eq!(loaded.compression_generation, 0);
        assert_eq!(loaded.compression_checkpoint, None);
    }

    #[test]
    fn records_only_effective_manual_compression() {
        let mut session = Session::new(PathBuf::from("/workspace"), None, "compression");
        session.messages = (0..10)
            .map(|index| Message::user(format!("message {index}")))
            .collect();
        assert!(session.replace_with_compressed_messages(vec![Message::user("summary")]));
        assert_eq!(session.compression_generation, 1);
        assert_eq!(session.messages.len(), 1);
        assert_eq!(
            session.compression_checkpoint,
            Some(CompressionCheckpoint {
                generation: 1,
                previous_message_count: 10,
                compressed_message_count: 1,
                created_at: session.compression_checkpoint.as_ref().unwrap().created_at,
            })
        );
        assert!(!session.replace_with_compressed_messages(session.messages.clone()));
        assert_eq!(session.compression_generation, 1);
    }
}
