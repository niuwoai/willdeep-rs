use super::{Session, SessionError, TitleSource};
use std::path::PathBuf;

/// In-memory baseline only. It never becomes part of the persisted schema.
#[derive(Clone, Debug)]
pub(super) struct Metadata {
    title: (String, TitleSource),
    pinned_at: Option<u64>,
    model: Option<String>,
    profile: Option<String>,
    config: Option<PathBuf>,
    goal: Option<String>,
    execution: [u8; 32],
}

impl Metadata {
    pub(super) fn refresh_execution(&mut self, saved: &Session) -> Result<(), SessionError> {
        self.execution = super::execution_state::fingerprint(saved)?;
        Ok(())
    }

    pub(super) fn capture(session: &Session) -> Result<Self, SessionError> {
        Ok(Self {
            title: (session.title.clone(), session.title_source),
            pinned_at: session.pinned_at,
            model: session.model.clone(),
            profile: session.profile.clone(),
            config: session.config.clone(),
            goal: session.goal.clone(),
            execution: super::execution_state::fingerprint(session)?,
        })
    }

    pub(super) fn merge(
        &self,
        proposed: &mut Session,
        latest: &Session,
    ) -> Result<(), SessionError> {
        // Validate all conflicts before changing the caller's snapshot.
        let mut merged = Self::capture(proposed)?;
        let current = Self::capture(latest)?;
        let retain_latest_execution = merged.execution == self.execution;
        if !retain_latest_execution
            && current.execution != self.execution
            && current.execution != merged.execution
        {
            return Err(SessionError::ConcurrentUpdate("execution"));
        }
        macro_rules! reconcile {
            ($field:ident) => {
                if merged.$field == self.$field {
                    merged.$field = current.$field;
                } else if current.$field != self.$field && current.$field != merged.$field {
                    return Err(SessionError::ConcurrentUpdate(stringify!($field)));
                }
            };
        }
        if merged.title.1 != TitleSource::User && current.title.1 == TitleSource::User {
            merged.title = current.title;
        } else {
            reconcile!(title);
        }
        reconcile!(pinned_at);
        reconcile!(model);
        reconcile!(profile);
        reconcile!(config);
        reconcile!(goal);
        if retain_latest_execution {
            super::execution_state::copy(latest, proposed);
        }
        (proposed.title, proposed.title_source) = merged.title;
        proposed.pinned_at = merged.pinned_at;
        proposed.model = merged.model;
        proposed.profile = merged.profile;
        proposed.config = merged.config;
        proposed.goal = merged.goal;
        Ok(())
    }
}
