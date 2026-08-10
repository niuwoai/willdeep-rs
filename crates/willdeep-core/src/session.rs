use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
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
    #[serde(default)]
    pub config: Option<PathBuf>,
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
            config: None,
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
        config: None,
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
        session.config = Some(root.join("config.toml"));
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
    fn legacy_session_without_config_remains_readable() {
        let session = Session::new(PathBuf::from("/workspace"), None, "legacy");
        let mut value = serde_json::to_value(session).unwrap();
        value.as_object_mut().unwrap().remove("config");
        let loaded: Session = serde_json::from_value(value).unwrap();
        assert_eq!(loaded.config, None);
    }
}
