use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::Message;
use crate::types::Role;

pub const SESSION_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub version: u32,
    pub id: Uuid,
    pub title: String,
    pub workspace: PathBuf,
    pub profile: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub attention_read: BTreeSet<String>,
    #[serde(default)]
    pub runtime_event_cursor: u64,
    #[serde(default)]
    pub runtime_managed: bool,
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
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
            attention_read: BTreeSet::new(),
            runtime_event_cursor: 0,
            runtime_managed: false,
            swift_source: None,
        }
    }
}

/// Listing view of a session: only the fields the session pickers render, so a
/// directory scan never has to deserialize message history.
#[derive(Clone, Debug)]
pub struct SessionSummary {
    pub id: Uuid,
    pub title: String,
    pub workspace: PathBuf,
    pub created_at: u64,
    pub updated_at: u64,
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
        let Some(newest) = self.list()?.into_iter().max_by_key(|s| s.updated_at) else {
            return Ok(None);
        };
        self.load(newest.id).map(Some)
    }
    /// Summaries of every readable session, newest first. Only summary fields are
    /// decoded, and unchanged files are served from an mtime-keyed cache, so a
    /// large bridged directory does not cost a full parse per call.
    pub fn list(&self) -> Result<Vec<SessionSummary>, SessionError> {
        let mut scan = SummaryScan::default();
        scan.directory(&self.directory, SummarySource::Local);
        if let Some(directory) = swift_session_directory() {
            scan.directory(&directory, SummarySource::Swift);
        }
        let mut values = scan.finish();
        values.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        Ok(values)
    }
    pub fn save(&self, session: &mut Session) -> Result<(), SessionError> {
        std::fs::create_dir_all(&self.directory)?;
        session.updated_at = now();
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum SummarySource {
    Local,
    Swift,
}

/// Identity of a file revision. A summary cached under the same stamp is still
/// current, so the file needs neither a read nor a parse.
#[derive(Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    modified: Option<SystemTime>,
    len: u64,
}

/// `None` records a file that failed to parse, so a broken file is not re-read
/// on every listing.
struct CachedSummary {
    stamp: FileStamp,
    summary: Option<SessionSummary>,
}

fn summary_cache() -> &'static Mutex<HashMap<PathBuf, CachedSummary>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedSummary>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_summary_cache() -> std::sync::MutexGuard<'static, HashMap<PathBuf, CachedSummary>> {
    summary_cache()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

#[derive(Default)]
struct SummaryScan {
    values: Vec<SessionSummary>,
    ids: HashSet<Uuid>,
    scanned: Vec<PathBuf>,
    visited: HashSet<PathBuf>,
}

impl SummaryScan {
    fn directory(&mut self, directory: &Path, source: SummarySource) {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };
        self.scanned.push(directory.to_path_buf());
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            let stamp = FileStamp {
                modified: metadata.modified().ok(),
                len: metadata.len(),
            };
            self.visited.insert(path.clone());
            let cached = lock_summary_cache()
                .get(&path)
                .filter(|entry| entry.stamp == stamp)
                .map(|entry| entry.summary.clone());
            let summary = match cached {
                Some(summary) => summary,
                None => {
                    let summary = match source {
                        SummarySource::Local => local_summary(&path),
                        SummarySource::Swift => swift_summary(&path, &stamp),
                    };
                    lock_summary_cache().insert(
                        path,
                        CachedSummary {
                            stamp,
                            summary: summary.clone(),
                        },
                    );
                    summary
                }
            };
            if let Some(summary) = summary
                && self.ids.insert(summary.id)
            {
                self.values.push(summary);
            }
        }
    }
    fn finish(self) -> Vec<SessionSummary> {
        // Drop entries for files that vanished from the directories just scanned,
        // while leaving other roots' entries (other `WILLDEEP_HOME`s) alone.
        let mut cache = lock_summary_cache();
        cache.retain(|path, _| {
            self.visited.contains(path)
                || !path
                    .parent()
                    .is_some_and(|parent| self.scanned.iter().any(|scanned| scanned == parent))
        });
        drop(cache);
        self.values
    }
}

#[derive(Deserialize)]
struct LocalSummaryFields {
    version: u32,
    id: Uuid,
    title: String,
    workspace: PathBuf,
    created_at: u64,
    updated_at: u64,
}

fn local_summary(path: &Path) -> Option<SessionSummary> {
    let fields: LocalSummaryFields = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (fields.version == SESSION_VERSION).then_some(SessionSummary {
        id: fields.id,
        title: fields.title,
        workspace: fields.workspace,
        created_at: fields.created_at,
        updated_at: fields.updated_at,
    })
}

#[derive(Deserialize)]
struct SwiftWorkspaceLocation {
    path: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SwiftSummaryFields {
    id: Uuid,
    title: Option<String>,
    workspace_location: Option<SwiftWorkspaceLocation>,
    workspace_root_path: Option<String>,
}

/// Mirrors [`swift_session`]'s header mapping, including deriving both timestamps
/// from the file's modification time, but never touches `messages`.
fn swift_summary(path: &Path, stamp: &FileStamp) -> Option<SessionSummary> {
    let fields: SwiftSummaryFields = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let updated = stamp
        .modified
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_secs())
        .unwrap_or_default();
    Some(SessionSummary {
        id: fields.id,
        title: fields.title.unwrap_or_else(|| "Swift session".to_owned()),
        workspace: PathBuf::from(
            fields
                .workspace_location
                .and_then(|location| location.path)
                .or(fields.workspace_root_path)
                .unwrap_or_else(|| ".".to_owned()),
        ),
        created_at: updated,
        updated_at: updated,
    })
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
        created_at: updated,
        updated_at: updated,
        messages,
        attention_read: BTreeSet::new(),
        runtime_event_cursor: 0,
        runtime_managed: false,
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

    fn summary(store: &SessionStore, id: Uuid) -> Option<SessionSummary> {
        store
            .list()
            .unwrap()
            .into_iter()
            .find(|value| value.id == id)
    }

    #[test]
    fn lists_summaries_and_follows_file_changes() {
        let root = std::env::temp_dir().join(format!("willdeep-session-{}", Uuid::new_v4()));
        let store = SessionStore::new(&root);
        let mut session = Session::new(root.clone(), None, "listed session");
        session.messages.push(Message::user("hello"));
        store.save(&mut session).unwrap();

        let listed = summary(&store, session.id).expect("saved session is listed");
        assert_eq!(listed.title, "listed session");
        assert_eq!(listed.workspace, root);
        assert_eq!(listed.updated_at, session.updated_at);

        // A rewrite must invalidate the cached summary rather than serve the old title.
        session.title = "renamed session".to_owned();
        store.save(&mut session).unwrap();
        assert_eq!(
            summary(&store, session.id).map(|value| value.title),
            Some("renamed session".to_owned())
        );

        assert!(store.delete(session.id).unwrap());
        assert!(summary(&store, session.id).is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sorts_newest_first_and_resumes_latest() {
        let root = std::env::temp_dir().join(format!("willdeep-session-{}", Uuid::new_v4()));
        let store = SessionStore::new(&root);
        let mut older = Session::new(root.clone(), None, "older session");
        store.save(&mut older).unwrap();
        let mut newer = Session::new(root.clone(), None, "newer session");
        newer.messages.push(Message::user("newest content"));
        store.save(&mut newer).unwrap();
        // `save` stamps whole seconds, so force a distinct ordering key.
        older.updated_at = newer.updated_at.saturating_sub(60);
        let data = serde_json::to_vec_pretty(&older).unwrap();
        std::fs::write(
            root.join("sessions").join(format!("{}.json", older.id)),
            data,
        )
        .unwrap();

        let listed = store.list().unwrap();
        let older_index = listed.iter().position(|v| v.id == older.id).unwrap();
        let newer_index = listed.iter().position(|v| v.id == newer.id).unwrap();
        assert!(newer_index < older_index, "newest session must sort first");

        // `latest` must return full message history, not just the summary fields.
        let latest = store.latest().unwrap().expect("a session exists");
        assert_eq!(latest.id, newer.id);
        assert_eq!(latest.messages.len(), 1);
        assert_eq!(latest.messages[0].content, "newest content");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn skips_unreadable_and_unsupported_sessions() {
        let root = std::env::temp_dir().join(format!("willdeep-session-{}", Uuid::new_v4()));
        let store = SessionStore::new(&root);
        let mut session = Session::new(root.clone(), None, "supported session");
        store.save(&mut session).unwrap();
        let directory = root.join("sessions");

        let corrupt = Uuid::new_v4();
        std::fs::write(directory.join(format!("{corrupt}.json")), b"{not json").unwrap();
        let unsupported = Uuid::new_v4();
        let mut future = Session::new(root.clone(), None, "future session");
        future.id = unsupported;
        future.version = SESSION_VERSION + 1;
        std::fs::write(
            directory.join(format!("{unsupported}.json")),
            serde_json::to_vec_pretty(&future).unwrap(),
        )
        .unwrap();

        let listed = store.list().unwrap();
        assert!(listed.iter().any(|value| value.id == session.id));
        assert!(!listed.iter().any(|value| value.id == corrupt));
        assert!(
            !listed.iter().any(|value| value.id == unsupported),
            "a session `load` would reject must not be listed"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
