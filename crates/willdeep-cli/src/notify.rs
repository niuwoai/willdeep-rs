//! Outbound attention delivery.
//!
//! `[notifications]` is shared with WillDeep.app so one config file drives
//! both clients. The desktop app owns sound playback; this module owns the
//! CLI half — posting the webhook when a turn finishes and when the agent
//! parks on a human gate.
//!
//! Delivery is a courtesy ping, never part of the agent's critical path: it
//! runs detached, it cannot fail a turn, and a dead endpoint costs at most
//! one timeout on a background task.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use willdeep_core::{AttentionItem, AttentionSection, RuntimeStatus};

use crate::config::NotificationSettings;

/// Short enough that an unreachable endpoint never keeps a background task
/// alive long enough to be noticed.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// The detail line is a human hint, not a transcript. Truncating hard keeps
/// a long command or diff from riding out to a remote endpoint.
const MAX_DETAIL_CHARS: usize = 400;
const CLIENT_NAME: &str = "willdeep-cli";
const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationEvent {
    TaskCompleted,
    AttentionRequired,
}

/// The CLI's webhook contract. Kept flat and free of transcript content so
/// the same body is safe to send to a remote endpoint, not just to a
/// loopback listener.
#[derive(Clone, Debug, Serialize)]
pub struct WebhookPayload {
    pub event: NotificationEvent,
    pub client: &'static str,
    pub client_version: &'static str,
    pub workspace: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub title: String,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<RuntimeStatus>,
}

/// A disabled notifier is the common case (webhooks are opt-in), so the whole
/// dispatcher collapses to a `None` and every call becomes a no-op.
#[derive(Clone, Default)]
pub struct Notifier {
    inner: Option<Arc<Inner>>,
}

struct Inner {
    client: reqwest::Client,
    url: String,
    on_task_completed: bool,
    on_attention_required: bool,
    workspace: String,
    session_id: Mutex<Option<String>>,
    /// Runtime attention is level-triggered — the same open gate reappears in
    /// every snapshot — so only the first sighting of an id is worth a post.
    announced: Mutex<HashSet<String>>,
    /// Delivery is detached, so failures land here instead of being swallowed;
    /// the TUI drains this into its notice line once per refresh tick.
    last_error: Mutex<Option<String>>,
}

impl Notifier {
    /// Build a dispatcher from the shared config section. Returns a disabled
    /// notifier when the webhook is off or the URL cannot be used — config
    /// validation has already rejected a bad URL by this point, so a failure
    /// here means "stay quiet", never "abort the run".
    pub fn new(settings: &NotificationSettings, workspace: &Path) -> Self {
        if !settings.webhook_enabled.unwrap_or(false) {
            return Self::disabled();
        }
        let Some(url) = settings
            .webhook_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Self::disabled();
        };
        let Ok(client) = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(format!("{CLIENT_NAME}/{CLIENT_VERSION}"))
            .build()
        else {
            return Self::disabled();
        };
        Self {
            inner: Some(Arc::new(Inner {
                client,
                url: url.to_owned(),
                on_task_completed: settings.webhook_on_task_completed.unwrap_or(true),
                on_attention_required: settings.webhook_on_attention_required.unwrap_or(true),
                workspace: workspace.display().to_string(),
                session_id: Mutex::new(None),
                announced: Mutex::new(HashSet::new()),
                last_error: Mutex::new(None),
            })),
        }
    }

    pub fn disabled() -> Self {
        Self { inner: None }
    }

    pub fn is_enabled(&self) -> bool {
        self.inner.is_some()
    }

    /// The session id is only known after the session is resolved, which
    /// happens after the harness is built.
    pub fn set_session(&self, id: &str) {
        let Some(inner) = &self.inner else { return };
        if let Ok(mut slot) = inner.session_id.lock() {
            *slot = Some(id.to_owned());
        }
    }

    /// A turn finished. Edge-triggered by the caller, so no de-duplication.
    pub fn task_completed(&self, title: impl Into<String>, detail: impl Into<String>) {
        let Some(inner) = &self.inner else { return };
        if !inner.on_task_completed {
            return;
        }
        self.post(inner.payload(
            NotificationEvent::TaskCompleted,
            title.into(),
            detail.into(),
            Some(RuntimeStatus::Done),
        ));
    }

    /// The agent parked on a human gate. Edge-triggered by the caller — an
    /// approval prompt is raised once per request — so no de-duplication.
    pub fn attention_required(
        &self,
        title: impl Into<String>,
        detail: impl Into<String>,
        status: RuntimeStatus,
    ) {
        let Some(inner) = &self.inner else { return };
        if !inner.on_attention_required {
            return;
        }
        self.post(inner.payload(
            NotificationEvent::AttentionRequired,
            title.into(),
            detail.into(),
            Some(status),
        ));
    }

    /// Level-triggered counterpart for runtime snapshots: post only the items
    /// that actually need a human, and only the first time each one is seen.
    pub fn attention_snapshot(&self, items: &[AttentionItem]) {
        let Some(inner) = &self.inner else { return };
        if !inner.on_attention_required {
            return;
        }
        for item in items {
            if item.status.section() != Some(AttentionSection::NeedsYou) {
                continue;
            }
            let Ok(mut announced) = inner.announced.lock() else {
                return;
            };
            if !announced.insert(item.id.clone()) {
                continue;
            }
            drop(announced);
            self.post(inner.payload(
                NotificationEvent::AttentionRequired,
                item.title.clone(),
                item.detail.clone(),
                Some(item.status),
            ));
        }
    }

    /// Take the most recent delivery failure, if any. Callers surface this so
    /// a misconfigured webhook is visible instead of silently doing nothing.
    pub fn take_error(&self) -> Option<String> {
        let inner = self.inner.as_ref()?;
        inner.last_error.lock().ok()?.take()
    }

    fn post(&self, payload: WebhookPayload) {
        let Some(inner) = self.inner.clone() else {
            return;
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            inner.record_error("no async runtime available for webhook delivery".to_owned());
            return;
        };
        handle.spawn(async move {
            match inner.client.post(&inner.url).json(&payload).send().await {
                Ok(response) if response.status().is_success() => {}
                Ok(response) => {
                    inner.record_error(format!("webhook returned HTTP {}", response.status()));
                }
                Err(error) => inner.record_error(format!("webhook delivery failed: {error}")),
            }
        });
    }
}

impl Inner {
    fn payload(
        &self,
        event: NotificationEvent,
        title: String,
        detail: String,
        status: Option<RuntimeStatus>,
    ) -> WebhookPayload {
        WebhookPayload {
            event,
            client: CLIENT_NAME,
            client_version: CLIENT_VERSION,
            workspace: self.workspace.clone(),
            session_id: self
                .session_id
                .lock()
                .ok()
                .and_then(|slot| slot.as_ref().cloned()),
            title: clamp(&title),
            detail: clamp(&detail),
            status,
        }
    }

    fn record_error(&self, message: String) {
        if let Ok(mut slot) = self.last_error.lock() {
            *slot = Some(message);
        }
    }
}

fn clamp(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= MAX_DETAIL_CHARS {
        return trimmed.to_owned();
    }
    let head: String = trimmed.chars().take(MAX_DETAIL_CHARS).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_settings() -> NotificationSettings {
        NotificationSettings {
            webhook_enabled: Some(true),
            webhook_url: Some("http://127.0.0.1:8787/willdeep".to_owned()),
            ..NotificationSettings::default()
        }
    }

    fn needs_you(id: &str) -> AttentionItem {
        AttentionItem {
            id: id.to_owned(),
            source: willdeep_core::AttentionSource::DiffReview,
            title: "3 changed file(s) ready for review".to_owned(),
            detail: "M src/a.rs".to_owned(),
            status: RuntimeStatus::WaitingApproval,
            elapsed_millis: None,
        }
    }

    #[test]
    fn disabled_webhook_builds_a_silent_notifier() {
        let notifier = Notifier::new(&NotificationSettings::default(), Path::new("/tmp/ws"));
        assert!(!notifier.is_enabled());
        // Every entry point must stay callable on the disabled path.
        notifier.task_completed("done", "detail");
        notifier.attention_required("gate", "detail", RuntimeStatus::WaitingApproval);
        notifier.attention_snapshot(&[needs_you("diff-review:1")]);
        assert!(notifier.take_error().is_none());
    }

    #[test]
    fn enabled_webhook_requires_a_usable_url() {
        let mut settings = enabled_settings();
        settings.webhook_url = Some("   ".to_owned());
        assert!(!Notifier::new(&settings, Path::new("/tmp/ws")).is_enabled());
        assert!(Notifier::new(&enabled_settings(), Path::new("/tmp/ws")).is_enabled());
    }

    #[test]
    fn per_event_switches_are_honoured_independently() {
        let mut settings = enabled_settings();
        settings.webhook_on_task_completed = Some(false);
        settings.webhook_on_attention_required = Some(true);
        let notifier = Notifier::new(&settings, Path::new("/tmp/ws"));
        let inner = notifier.inner.as_ref().expect("enabled");
        assert!(!inner.on_task_completed);
        assert!(inner.on_attention_required);
    }

    #[test]
    fn snapshot_announces_each_gate_once_and_skips_non_gates() {
        let notifier = Notifier::new(&enabled_settings(), Path::new("/tmp/ws"));
        let inner = notifier.inner.clone().expect("enabled");
        let mut working = needs_you("background:1");
        working.status = RuntimeStatus::Working;

        notifier.attention_snapshot(&[needs_you("diff-review:1"), working]);
        notifier.attention_snapshot(&[needs_you("diff-review:1")]);

        let announced = inner.announced.lock().expect("lock");
        assert_eq!(announced.len(), 1);
        assert!(announced.contains("diff-review:1"));
    }

    #[test]
    fn payload_omits_transcript_and_truncates_long_detail() {
        let notifier = Notifier::new(&enabled_settings(), Path::new("/tmp/ws"));
        let inner = notifier.inner.as_ref().expect("enabled");
        inner.session_id.lock().expect("lock").replace("s-1".into());
        let payload = inner.payload(
            NotificationEvent::AttentionRequired,
            "  Approval required  ".to_owned(),
            "x".repeat(MAX_DETAIL_CHARS + 50),
            Some(RuntimeStatus::WaitingApproval),
        );

        assert_eq!(payload.title, "Approval required");
        assert_eq!(payload.detail.chars().count(), MAX_DETAIL_CHARS + 1);
        assert!(payload.detail.ends_with('…'));
        assert_eq!(payload.session_id.as_deref(), Some("s-1"));

        let json = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(json["event"], "attention_required");
        assert_eq!(json["status"], "waiting_approval");
        assert_eq!(json["client"], CLIENT_NAME);
    }

    #[test]
    fn delivery_without_a_runtime_records_an_error_instead_of_panicking() {
        let notifier = Notifier::new(&enabled_settings(), Path::new("/tmp/ws"));
        notifier.task_completed("Turn finished", "2 turns");
        let error = notifier.take_error().expect("error recorded");
        assert!(error.contains("no async runtime"));
        assert!(notifier.take_error().is_none());
    }

    #[tokio::test]
    async fn posts_a_real_request_to_a_local_listener() {
        use std::sync::mpsc;

        let (tx, rx) = mpsc::channel::<serde_json::Value>();
        let tx = Arc::new(Mutex::new(tx));
        let app = axum::Router::new().route(
            "/willdeep",
            axum::routing::post(move |body: axum::Json<serde_json::Value>| {
                let tx = tx.clone();
                async move {
                    if let Ok(tx) = tx.lock() {
                        let _ = tx.send(body.0);
                    }
                    axum::http::StatusCode::NO_CONTENT
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let mut settings = enabled_settings();
        settings.webhook_url = Some(format!("http://127.0.0.1:{port}/willdeep"));
        let notifier = Notifier::new(&settings, Path::new("/tmp/ws"));
        notifier.set_session("session-42");
        notifier.attention_required(
            "Approval required",
            "run_command: rm -rf build",
            RuntimeStatus::WaitingApproval,
        );

        let received =
            tokio::task::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(10)))
                .await
                .expect("join")
                .expect("webhook body delivered");

        assert_eq!(received["event"], "attention_required");
        assert_eq!(received["status"], "waiting_approval");
        assert_eq!(received["session_id"], "session-42");
        assert_eq!(received["workspace"], "/tmp/ws");
        assert_eq!(received["detail"], "run_command: rm -rf build");
        assert_eq!(received["client_version"], CLIENT_VERSION);
        assert!(notifier.take_error().is_none(), "a 2xx is not an error");
    }

    #[tokio::test]
    async fn failed_delivery_surfaces_an_error_without_breaking_the_caller() {
        let mut settings = enabled_settings();
        // Reserved TEST-NET-1 address: routable nowhere, so this always fails.
        settings.webhook_url = Some("http://192.0.2.1:9/willdeep".to_owned());
        let notifier = Notifier::new(&settings, Path::new("/tmp/ws"));
        notifier.task_completed("Turn finished", "2 turns");

        let mut error = None;
        for _ in 0..60 {
            if let Some(found) = notifier.take_error() {
                error = Some(found);
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            error.is_some_and(|value| value.contains("webhook delivery failed")),
            "expected a recorded delivery failure"
        );
    }
}
