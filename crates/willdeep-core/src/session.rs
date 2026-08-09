use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::Message;

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
        let session: Session = serde_json::from_slice(&std::fs::read(self.path(id))?)?;
        if session.version != SESSION_VERSION {
            return Err(SessionError::Version(session.version));
        }
        Ok(session)
    }
    pub fn latest(&self) -> Result<Option<Session>, SessionError> {
        Ok(self.list()?.into_iter().max_by_key(|s| s.updated_at))
    }
    pub fn list(&self) -> Result<Vec<Session>, SessionError> {
        let Ok(entries) = std::fs::read_dir(&self.directory) else {
            return Ok(Vec::new());
        };
        let mut values = Vec::new();
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
    fn path(&self, id: Uuid) -> PathBuf {
        self.directory.join(format!("{id}.json"))
    }
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
        session.messages.push(Message::user("hello"));
        store.save(&mut session).unwrap();
        assert_eq!(store.load(session.id).unwrap().messages.len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }
}
