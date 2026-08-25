use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::session_title::{self, TitleSource};
use crate::types::Message;
use crate::types::Role;

pub const SESSION_VERSION: u32 = 1;

/// 默认家目录名，`~/<DEFAULT_HOME_DIRECTORY>` 即未设置 `WILLDEEP_HOME` 时 CLI 用的家目录。
/// 只有 macOS 才有桌面 App 会话桥接，其他平台仅测试引用它。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const DEFAULT_HOME_DIRECTORY: &str = ".willdeep";

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
    /// 标题从哪来的，决定自动流程还能不能改它。缺省 `Legacy`：这个字段出现
    /// 之前的会话文件里没有它，而它们的标题只能靠占位符判定来接管。
    #[serde(default)]
    pub title_source: TitleSource,
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
        let derived = session_title::derive_from_prompt(prompt, false);
        Self {
            version: SESSION_VERSION,
            id: Uuid::new_v4(),
            title: derived.clone(),
            // 占位符不算「派生过」：`Session::new` 在 TUI 里是先于任何提示词
            // 建出来的，此时标题只是个名字位，得留给后续轮次接管。
            title_source: if session_title::is_placeholder(&derived) {
                TitleSource::Legacy
            } else {
                TitleSource::Derived
            },
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

/// 会话列表视图需要的元数据快照，不携带消息正文。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionDigest {
    pub id: Uuid,
    pub title: String,
    pub workspace: PathBuf,
    pub created_at: u64,
    pub updated_at: u64,
    pub pinned_at: Option<u64>,
    /// 是否存在非空的用户输入（正文非空或带附件）。
    pub has_user_input: bool,
    /// 消息条数。列表视图靠它区分「聊过的」和「点开就没再回来的」。
    pub message_count: usize,
    /// 会话文件由 Xedit（macOS 桌面版）写出，rs 这边是只读桥接。
    pub bridged: bool,
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
        } else if let Some(path) = swift_session_directory(&self.directory)
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
        match self.digests().into_iter().max_by_key(|s| s.updated_at) {
            Some(digest) => Ok(Some(self.load(digest.id)?)),
            None => Ok(None),
        }
    }
    /// 列出所有会话的元数据。只解析列表视图需要的字段，不物化消息正文，
    /// 并按 (mtime, size) 缓存解析结果——会话目录动辄上百个文件、几十 MB，
    /// 每次轮询全量反序列化会把 CPU 吃满。需要完整会话请用 [`SessionStore::load`]。
    pub fn digests(&self) -> Vec<SessionDigest> {
        let mut values = Vec::new();
        let mut visited = HashSet::new();
        let mut scanned = HashSet::from([self.directory.clone()]);
        for path in json_files(&self.directory) {
            if let Some(digest) = cached_digest(&path, local_digest) {
                visited.insert(path);
                values.push(digest);
            }
        }
        if let Some(directory) = swift_session_directory(&self.directory) {
            for path in json_files(&directory) {
                let Some(digest) = cached_digest(&path, swift_digest) else {
                    continue;
                };
                visited.insert(path);
                if !values.iter().any(|existing| existing.id == digest.id) {
                    values.push(digest);
                }
            }
            scanned.insert(directory);
        }
        // 只清理本次扫过的目录里已消失的文件；别动其它 SessionStore 的缓存。
        digest_cache()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retain(|path, _| {
                visited.contains(path)
                    || !path.parent().is_some_and(|parent| scanned.contains(parent))
            });
        values.sort_by_key(|digest| std::cmp::Reverse(digest.updated_at));
        values
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
        if let Some(directory) = swift_session_directory(&self.directory)
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

struct CachedDigest {
    fingerprint: (u64, u64),
    digest: SessionDigest,
}

fn digest_cache() -> &'static Mutex<HashMap<PathBuf, CachedDigest>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedDigest>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn json_files(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect()
}

/// 文件指纹：修改时间（纳秒）+ 字节数。任一变化都重新解析。
fn fingerprint(metadata: &std::fs::Metadata) -> (u64, u64) {
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_nanos() as u64)
        .unwrap_or_default();
    (modified, metadata.len())
}

fn cached_digest(
    path: &Path,
    parse: fn(&Path, &std::fs::Metadata) -> Option<SessionDigest>,
) -> Option<SessionDigest> {
    let metadata = std::fs::metadata(path).ok()?;
    let fingerprint = fingerprint(&metadata);
    if let Some(cached) = digest_cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(path)
        .filter(|cached| cached.fingerprint == fingerprint)
    {
        return Some(cached.digest.clone());
    }
    let digest = parse(path, &metadata)?;
    digest_cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(
            path.to_path_buf(),
            CachedDigest {
                fingerprint,
                digest: digest.clone(),
            },
        );
    Some(digest)
}

/// 只声明列表视图用得到的字段；`messages` 用探针结构跳过正文的分配。
#[derive(Deserialize)]
struct LocalDigestProbe {
    id: Uuid,
    title: String,
    workspace: PathBuf,
    created_at: u64,
    updated_at: u64,
    #[serde(default)]
    pinned_at: Option<u64>,
    #[serde(default)]
    messages: Vec<MessageProbe>,
}

#[derive(Deserialize)]
struct MessageProbe {
    #[serde(default)]
    role: String,
    #[serde(default, deserialize_with = "non_empty_text")]
    content: bool,
    #[serde(default)]
    attachments: Option<Vec<serde::de::IgnoredAny>>,
}

impl MessageProbe {
    fn is_user_input(&self) -> bool {
        self.role == "user"
            && (self.content
                || self
                    .attachments
                    .as_ref()
                    .is_some_and(|values| !values.is_empty()))
    }
}

/// 把任意 JSON 值折叠成"是否为非空文本"，避免为消息正文分配 String。
fn non_empty_text<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct TextVisitor;
    impl<'de> serde::de::Visitor<'de> for TextVisitor {
        type Value = bool;
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("message content")
        }
        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<bool, E> {
            Ok(!value.trim().is_empty())
        }
        fn visit_unit<E: serde::de::Error>(self) -> Result<bool, E> {
            Ok(false)
        }
        fn visit_none<E: serde::de::Error>(self) -> Result<bool, E> {
            Ok(false)
        }
        fn visit_some<D: serde::Deserializer<'de>>(
            self,
            deserializer: D,
        ) -> Result<bool, D::Error> {
            deserializer.deserialize_any(self)
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<bool, A::Error> {
            let mut present = false;
            while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
                present = true;
            }
            Ok(present)
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<bool, A::Error> {
            let mut present = false;
            while map
                .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                .is_some()
            {
                present = true;
            }
            Ok(present)
        }
    }
    deserializer.deserialize_any(TextVisitor)
}

fn local_digest(path: &Path, _metadata: &std::fs::Metadata) -> Option<SessionDigest> {
    let probe: LocalDigestProbe = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    Some(SessionDigest {
        id: probe.id,
        title: probe.title,
        workspace: probe.workspace,
        created_at: probe.created_at,
        updated_at: probe.updated_at,
        pinned_at: probe.pinned_at,
        has_user_input: probe.messages.iter().any(MessageProbe::is_user_input),
        message_count: probe.messages.len(),
        bridged: false,
    })
}

/// Xedit（Swift）写出的会话文件：字段名是 camelCase，时间以文件 mtime 为准。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SwiftDigestProbe {
    id: Uuid,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    workspace_location: Option<SwiftWorkspaceLocation>,
    #[serde(default)]
    workspace_root_path: Option<String>,
    #[serde(default)]
    pinned_at: Option<String>,
    #[serde(default)]
    messages: Vec<MessageProbe>,
}

#[derive(Deserialize)]
struct SwiftWorkspaceLocation {
    #[serde(default)]
    path: Option<String>,
}

fn swift_digest(path: &Path, metadata: &std::fs::Metadata) -> Option<SessionDigest> {
    let probe: SwiftDigestProbe = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let workspace = probe
        .workspace_location
        .and_then(|location| location.path)
        .or(probe.workspace_root_path)
        .unwrap_or_else(|| ".".to_owned());
    let updated = fingerprint(metadata).0 / 1_000_000_000;
    Some(SessionDigest {
        id: probe.id,
        title: probe.title.unwrap_or_else(|| "Swift session".to_owned()),
        workspace: PathBuf::from(workspace),
        created_at: updated,
        updated_at: updated,
        pinned_at: probe.pinned_at.as_deref().and_then(parse_iso8601),
        has_user_input: probe.messages.iter().any(MessageProbe::is_user_input),
        message_count: probe.messages.len(),
        bridged: true,
    })
}

fn swift_session_directory(store_directory: &Path) -> Option<PathBuf> {
    swift_bridge_directory(
        store_directory,
        std::env::var_os("HOME").map(PathBuf::from).as_deref(),
    )
}

/// 桌面 App 把自己的 Session 写在固定的 Application Support 目录里，只有默认
/// 家目录（`~/.willdeep`）下的 Store 才代表"这台机器上这个用户的 CLI 历史"，
/// 才该把它们合并进来。任何别的家目录——测试、`WILLDEEP_HOME`、沙箱——必须保持
/// 自足：既不读别人的 Session，也不为解析它们付出代价（真实机器上这个目录可以
/// 有几百个文件、几十 MB，每次 list 都要全量解析）。
#[cfg(target_os = "macos")]
fn swift_bridge_directory(store_directory: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let home = home?;
    (store_directory == home.join(DEFAULT_HOME_DIRECTORY).join("sessions"))
        .then(|| home.join("Library/Application Support/WillDeep/agent-sessions"))
}

#[cfg(not(target_os = "macos"))]
fn swift_bridge_directory(_store_directory: &Path, _home: Option<&Path>) -> Option<PathBuf> {
    None
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
        // Xedit 有自己的两级标题流程，桥接过来的标题按既成事实读，不重算。
        title_source: TitleSource::Legacy,
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

/// 供 CLI 的 webhook 复用：Xedit 的 `willdeep.webhook.v1` 用同一种
/// ISO8601 UTC 表示，两端必须逐字一致。
pub fn format_iso8601(timestamp: u64) -> String {
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
    fn digest_reports_user_input_and_reuses_the_cache_until_the_file_changes() {
        let root = std::env::temp_dir().join(format!("willdeep-session-digest-{}", Uuid::new_v4()));
        let store = SessionStore::new(&root);
        let mut session = Session::new(root.clone(), None, "New session");
        store.save(&mut session).unwrap();
        let id = session.id;
        let digest = || {
            store
                .digests()
                .into_iter()
                .find(|digest| digest.id == id)
                .expect("digest for the saved session")
        };
        assert!(!digest().has_user_input);

        session
            .messages
            .push(Message::assistant("welcome", Vec::new()));
        store.save(&mut session).unwrap();
        assert!(!digest().has_user_input);

        // 只有附件、没有正文，也算用户输入。
        session.messages.push(Message::user_with_attachments(
            "",
            vec![crate::MessageAttachment::Text {
                name: "notes.txt".to_owned(),
                content: "context".to_owned(),
            }],
        ));
        store.save(&mut session).unwrap();
        assert!(digest().has_user_input);

        session.messages.push(Message::user("hello"));
        store.save(&mut session).unwrap();
        let current = digest();
        assert!(current.has_user_input);
        assert_eq!(current.title, session.title);
        assert_eq!(current.updated_at, session.updated_at);

        // 文件没动时走缓存，结果必须与上一轮完全一致。
        assert_eq!(digest(), current);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn digest_drops_cache_entries_for_deleted_sessions() {
        let root = std::env::temp_dir().join(format!("willdeep-session-prune-{}", Uuid::new_v4()));
        let store = SessionStore::new(&root);
        let mut session = Session::new(root.clone(), None, "temporary");
        store.save(&mut session).unwrap();
        let path = store.path(session.id);
        assert!(store.digests().iter().any(|digest| digest.id == session.id));
        assert!(
            digest_cache()
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .contains_key(&path)
        );
        assert!(store.delete(session.id).unwrap());
        assert!(!store.digests().iter().any(|digest| digest.id == session.id));
        assert!(
            !digest_cache()
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .contains_key(&path)
        );
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
    fn bridges_desktop_sessions_only_for_the_default_home() {
        let home = PathBuf::from("/Users/tester");
        let bridged = home.join("Library/Application Support/WillDeep/agent-sessions");

        let default_home_store = home.join(DEFAULT_HOME_DIRECTORY).join("sessions");
        assert_eq!(
            swift_bridge_directory(&default_home_store, Some(&home)),
            cfg!(target_os = "macos").then_some(bridged)
        );

        // 自定义家目录（测试、WILLDEEP_HOME、沙箱）必须自足，不去碰桌面 App 的 Session。
        for foreign in [
            PathBuf::from("/tmp/willdeep-test-home/sessions"),
            home.join("other-home").join("sessions"),
        ] {
            assert_eq!(swift_bridge_directory(&foreign, Some(&home)), None);
        }
        assert_eq!(swift_bridge_directory(&default_home_store, None), None);
    }

    /// Xedit 写的会话文件用 camelCase、时间戳是 ISO8601、正文键叫 `content`。
    /// 列表视图要从里面读出标题、工作区和消息数，并且认出它是桥接来的——
    /// 桥接会话在 rs 这边是只读的，UI 得能把这件事标出来。
    #[test]
    fn desktop_session_files_yield_a_readable_digest() {
        let root = std::env::temp_dir().join(format!("willdeep-swift-digest-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let id = Uuid::new_v4();
        let path = root.join(format!("{id}.json"));
        std::fs::write(
            &path,
            serde_json::json!({
                "id": id,
                "title": "代码提交一下。",
                "workspaceLocation": {"path": "/Users/tester/Sites/willdeep-rs"},
                "pinnedAt": "2026-08-11T00:00:00Z",
                "messages": [
                    {"role": "user", "content": "代码提交一下。"},
                    {"role": "assistant", "content": "好的"},
                ],
            })
            .to_string(),
        )
        .unwrap();

        let digest = cached_digest(&path, swift_digest).expect("Swift digest");
        assert_eq!(digest.id, id);
        assert_eq!(digest.title, "代码提交一下。");
        assert_eq!(
            digest.workspace,
            PathBuf::from("/Users/tester/Sites/willdeep-rs")
        );
        assert_eq!(digest.message_count, 2);
        assert!(digest.has_user_input);
        assert!(digest.bridged, "desktop sessions must be marked read-only");
        assert_eq!(digest.pinned_at, Some(1_786_406_400));

        // 没有标题的会话仍要出现在列表里，只是叫占位名——不能整条被丢掉。
        let untitled = Uuid::new_v4();
        let untitled_path = root.join(format!("{untitled}.json"));
        std::fs::write(
            &untitled_path,
            serde_json::json!({"id": untitled, "messages": []}).to_string(),
        )
        .unwrap();
        let digest = cached_digest(&untitled_path, swift_digest).expect("untitled Swift digest");
        assert_eq!(digest.title, "Swift session");
        assert_eq!(digest.message_count, 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// 桥接会话读进来时按 `Legacy` 记：Xedit 有自己的两级标题流程，rs 这边
    /// 不该在别人的会话上再跑一遍自动改名。
    #[test]
    fn bridged_sessions_keep_their_desktop_title() {
        let root = std::env::temp_dir().join(format!("willdeep-swift-load-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let id = Uuid::new_v4();
        let path = root.join(format!("{id}.json"));
        std::fs::write(
            &path,
            serde_json::json!({
                "id": id,
                "title": "排查 CPU 负载",
                "workspaceRootPath": "/Users/tester/Sites/tokenhub",
                "messages": [{"role": "user", "content": "top 一下"}],
            })
            .to_string(),
        )
        .unwrap();

        let session = swift_session(&path).expect("Swift session");
        assert_eq!(session.title, "排查 CPU 负载");
        assert_eq!(session.title_source, TitleSource::Legacy);
        assert_eq!(session.swift_source.as_deref(), Some(path.as_path()));
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
