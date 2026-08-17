//! Outbound attention delivery.
//!
//! `[notifications]` is shared with WillDeep.app so one config file drives
//! both clients. The desktop app owns sound playback; this module owns the
//! CLI half — posting the webhook when a turn finishes and when the agent
//! parks on a human gate.
//!
//! The wire format is **not** ours to invent: WillDeep.app already ships
//! `willdeep.webhook.v1` (see `Xedit/AgentAttentionSettings.swift`), and its
//! own receiver sniffs the vendor out of the headers and body. A CLI event
//! must therefore be indistinguishable from an app event apart from the
//! fields that legitimately identify the executor, so one endpoint can serve
//! both clients with one parser.
//!
//! Delivery is a courtesy ping, never part of the agent's critical path: it
//! runs detached, it cannot fail a turn, and a dead endpoint costs at most
//! one timeout on a background task.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use willdeep_core::{AttentionItem, AttentionSection, RuntimeStatus, format_iso8601};

use crate::config::NotificationSettings;

/// Matches `URLSessionConfiguration.timeoutIntervalForRequest` on the app side
/// so a slow endpoint behaves the same whichever client is talking to it.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// `summary` is a human hint, not a transcript. Truncating hard keeps a long
/// command or diff from riding out to a remote endpoint.
const MAX_SUMMARY_CHARS: usize = 400;

/// The app's `AgentWebhookFormatDetector` keys off these exact strings.
const SCHEMA_VERSION: &str = "willdeep.webhook.v1";
const SOURCE: &str = "willdeep";
const APPLICATION: &str = "WillDeep";
/// `executor_kind` is derived from `executor` by the app's detector, which
/// resolves any value containing "willdeep" to the `willdeep` vendor. Naming
/// the CLI here is what tells a receiver which client sent the event without
/// breaking that classification.
const EXECUTOR: &str = "willdeep-cli";
const EXECUTOR_KIND: &str = "willdeep";
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationEvent {
    TaskCompleted,
    AttentionRequired,
}

impl NotificationEvent {
    fn event(self) -> &'static str {
        match self {
            Self::TaskCompleted => "task_completed",
            Self::AttentionRequired => "attention_required",
        }
    }

    /// Legacy alias the app emits alongside `event` so hook receivers written
    /// for Codex-style payloads keep working.
    fn compatibility_type(self) -> &'static str {
        match self {
            Self::TaskCompleted => "agent-turn-complete",
            Self::AttentionRequired => "approval-requested",
        }
    }

    /// The Claude-Code-style hook name. The app picks `PermissionRequest` only
    /// for a Codex executor; the CLI is always the `willdeep` executor, so an
    /// attention event is always `Notification`.
    fn hook_event_name(self) -> &'static str {
        match self {
            Self::TaskCompleted => "Stop",
            Self::AttentionRequired => "Notification",
        }
    }
}

/// `willdeep.webhook.v1`. Field names and ordering mirror the app's
/// `AgentAttentionWebhookEvent`; `thread-id` really is hyphenated on the wire
/// (it is the Codex compatibility alias), so do not "fix" it to `thread_id`.
#[derive(Clone, Debug, Serialize)]
pub struct WebhookPayload {
    pub schema_version: &'static str,
    pub source: &'static str,
    pub event: &'static str,
    pub r#type: &'static str,
    pub hook_event_name: &'static str,
    pub occurred_at: String,
    pub application: &'static str,
    pub app_version: &'static str,
    pub runtime_id: Option<String>,
    pub session_id: Option<String>,
    #[serde(rename = "thread-id")]
    pub thread_id: Option<String>,
    pub session_title: Option<String>,
    pub executor: &'static str,
    pub executor_kind: &'static str,
    pub status: String,
    pub task_id: Option<String>,
    pub attention_kind: Option<String>,
    pub notification_type: Option<&'static str>,
    pub message: Option<String>,
    pub summary: Option<String>,
}

/// What the agent is waiting on, in the app's `AgentToolCallStatus` vocabulary.
/// `notification_type` is derived from these strings on both sides, so the
/// spellings have to match the Swift enum's raw values exactly.
pub fn status_label(status: RuntimeStatus) -> &'static str {
    match status {
        RuntimeStatus::WaitingApproval => "pendingApproval",
        RuntimeStatus::WaitingAnswer => "awaitingUserAnswer",
        RuntimeStatus::Blocked => "blocked",
        RuntimeStatus::Failed => "failed",
        RuntimeStatus::Working => "running",
        RuntimeStatus::Done => "succeeded",
        RuntimeStatus::Cancelled => "denied",
        RuntimeStatus::Idle => "idle",
        RuntimeStatus::Unknown => "unknown",
    }
}

/// Mirrors `AgentAttentionWebhookClient.notificationType`: only attention
/// events carry one, and the bucket is chosen from the status string.
fn notification_type(event: NotificationEvent, status: &str) -> Option<&'static str> {
    if event != NotificationEvent::AttentionRequired {
        return None;
    }
    Some(match status {
        "pendingApproval" | "awaitingConfirm" => "permission_prompt",
        "awaitingUserAnswer" => "elicitation_dialog",
        _ => "idle_prompt",
    })
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
    session: Mutex<Option<SessionIdentity>>,
    /// Runtime attention is level-triggered — the same open gate reappears in
    /// every snapshot — so only the first sighting of an id is worth a post.
    announced: Mutex<HashSet<String>>,
    /// Delivery is detached, so failures land here instead of being swallowed;
    /// the TUI drains this into its notice line once per refresh tick.
    last_error: Mutex<Option<String>>,
}

#[derive(Clone, Debug)]
struct SessionIdentity {
    id: String,
    title: Option<String>,
}

impl Notifier {
    /// Build a dispatcher from the shared config section. Returns a disabled
    /// notifier when the webhook is off or the URL cannot be used — config
    /// validation has already rejected a bad URL by this point, so a failure
    /// here means "stay quiet", never "abort the run".
    pub fn new(settings: &NotificationSettings) -> Self {
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
        // Mirrors the app's User-Agent shape. The detector resolves any agent
        // string containing "willdeep" to this vendor, so keeping the prefix
        // is what makes a CLI event self-identifying at header level.
        let user_agent = format!("some.im/WillDeep-{APP_VERSION} ({EXECUTOR})");
        let Ok(client) = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(user_agent)
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
                session: Mutex::new(None),
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

    /// The session is only known after it is resolved, which happens after the
    /// harness is built.
    pub fn set_session(&self, id: &str, title: Option<&str>) {
        let Some(inner) = &self.inner else { return };
        if let Ok(mut slot) = inner.session.lock() {
            *slot = Some(SessionIdentity {
                id: id.to_owned(),
                title: title
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned),
            });
        }
    }

    /// A turn finished. Edge-triggered by the caller, so no de-duplication.
    pub fn task_completed(&self, summary: impl Into<String>) {
        let Some(inner) = &self.inner else { return };
        if !inner.on_task_completed {
            return;
        }
        self.post(inner.payload(
            NotificationEvent::TaskCompleted,
            status_label(RuntimeStatus::Done),
            None,
            None,
            Some(summary.into()),
        ));
    }

    /// The agent parked on a human gate. Edge-triggered by the caller — an
    /// approval prompt is raised once per request — so no de-duplication.
    pub fn attention_required(
        &self,
        status: RuntimeStatus,
        attention_kind: &str,
        summary: impl Into<String>,
    ) {
        let Some(inner) = &self.inner else { return };
        if !inner.on_attention_required {
            return;
        }
        self.post(inner.payload(
            NotificationEvent::AttentionRequired,
            status_label(status),
            None,
            Some(attention_kind.to_owned()),
            Some(summary.into()),
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
                status_label(item.status),
                Some(item.id.clone()),
                Some(format!("{:?}", item.source).to_lowercase()),
                Some(format!("{} · {}", item.title, item.detail)),
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
            let request = inner
                .client
                .post(&inner.url)
                // The app sets these so a receiver can route on headers alone,
                // without parsing the body first.
                .header("X-App-Version", APP_VERSION)
                .header("X-Agent-Source", SOURCE)
                .header("X-Agent-Executor", EXECUTOR_KIND)
                .header("X-Webhook-Schema", SCHEMA_VERSION)
                .header("X-Agent-Event", payload.event)
                .json(&payload);
            match request.send().await {
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
        status: &'static str,
        task_id: Option<String>,
        attention_kind: Option<String>,
        summary: Option<String>,
    ) -> WebhookPayload {
        let session = self.session.lock().ok().and_then(|slot| slot.clone());
        let summary = summary.map(|value| clamp(&value));
        WebhookPayload {
            schema_version: SCHEMA_VERSION,
            source: SOURCE,
            event: event.event(),
            r#type: event.compatibility_type(),
            hook_event_name: event.hook_event_name(),
            occurred_at: format_iso8601(now_unix_seconds()),
            application: APPLICATION,
            app_version: APP_VERSION,
            // The app fills all three from one runtime id; a CLI session is
            // the same identity under all three names.
            runtime_id: session.as_ref().map(|value| value.id.clone()),
            session_id: session.as_ref().map(|value| value.id.clone()),
            thread_id: session.as_ref().map(|value| value.id.clone()),
            session_title: session.and_then(|value| value.title),
            executor: EXECUTOR,
            executor_kind: EXECUTOR_KIND,
            notification_type: notification_type(event, status),
            status: status.to_owned(),
            task_id,
            attention_kind,
            // `message` is the app's attention-only alias for `summary`.
            message: (event == NotificationEvent::AttentionRequired)
                .then(|| summary.clone())
                .flatten(),
            summary,
        }
    }

    fn record_error(&self, message: String) {
        if let Ok(mut slot) = self.last_error.lock() {
            *slot = Some(message);
        }
    }
}

fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default()
}

fn clamp(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= MAX_SUMMARY_CHARS {
        return trimmed.to_owned();
    }
    let head: String = trimmed.chars().take(MAX_SUMMARY_CHARS).collect();
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
        let notifier = Notifier::new(&NotificationSettings::default());
        assert!(!notifier.is_enabled());
        // Every entry point must stay callable on the disabled path.
        notifier.task_completed("done");
        notifier.attention_required(RuntimeStatus::WaitingApproval, "approval", "detail");
        notifier.attention_snapshot(&[needs_you("diff-review:1")]);
        assert!(notifier.take_error().is_none());
    }

    #[test]
    fn enabled_webhook_requires_a_usable_url() {
        let mut settings = enabled_settings();
        settings.webhook_url = Some("   ".to_owned());
        assert!(!Notifier::new(&settings).is_enabled());
        assert!(Notifier::new(&enabled_settings()).is_enabled());
    }

    #[test]
    fn per_event_switches_are_honoured_independently() {
        let mut settings = enabled_settings();
        settings.webhook_on_task_completed = Some(false);
        settings.webhook_on_attention_required = Some(true);
        let notifier = Notifier::new(&settings);
        let inner = notifier.inner.as_ref().expect("enabled");
        assert!(!inner.on_task_completed);
        assert!(inner.on_attention_required);
    }

    #[test]
    fn snapshot_announces_each_gate_once_and_skips_non_gates() {
        let notifier = Notifier::new(&enabled_settings());
        let inner = notifier.inner.clone().expect("enabled");
        let mut working = needs_you("background:1");
        working.status = RuntimeStatus::Working;

        notifier.attention_snapshot(&[needs_you("diff-review:1"), working]);
        notifier.attention_snapshot(&[needs_you("diff-review:1")]);

        let announced = inner.announced.lock().expect("lock");
        assert_eq!(announced.len(), 1);
        assert!(announced.contains("diff-review:1"));
    }

    /// Locks the contract against `Xedit/AgentAttentionSettings.swift`. The
    /// app's receiver classifies a payload by these exact keys and values, so
    /// a rename on either side is a wire break, not a refactor.
    #[test]
    fn payload_matches_the_willdeep_webhook_v1_envelope() {
        let notifier = Notifier::new(&enabled_settings());
        notifier.set_session("runtime-42", Some("Ship the release"));
        let inner = notifier.inner.as_ref().expect("enabled");
        let payload = inner.payload(
            NotificationEvent::TaskCompleted,
            status_label(RuntimeStatus::Done),
            Some("job-42".to_owned()),
            None,
            Some("job-42 · succeeded".to_owned()),
        );
        let json = serde_json::to_value(&payload).expect("serialize");

        assert_eq!(json["schema_version"], "willdeep.webhook.v1");
        assert_eq!(json["source"], "willdeep");
        assert_eq!(json["event"], "task_completed");
        assert_eq!(json["type"], "agent-turn-complete");
        assert_eq!(json["hook_event_name"], "Stop");
        assert_eq!(json["application"], "WillDeep");
        assert_eq!(json["executor"], "willdeep-cli");
        assert_eq!(json["executor_kind"], "willdeep");
        assert_eq!(json["status"], "succeeded");
        assert_eq!(json["task_id"], "job-42");
        assert_eq!(json["session_title"], "Ship the release");
        // All three identity aliases come from one id, and `thread-id` keeps
        // its hyphen because that is the Codex-compatible spelling.
        assert_eq!(json["runtime_id"], "runtime-42");
        assert_eq!(json["session_id"], "runtime-42");
        assert_eq!(json["thread-id"], "runtime-42");
        // `notification_type` and `message` are attention-only.
        assert!(json["notification_type"].is_null());
        assert!(json["message"].is_null());
        assert_eq!(json["summary"], "job-42 · succeeded");
        // ISO8601 UTC, the same shape Xedit writes.
        let occurred_at = json["occurred_at"].as_str().expect("occurred_at");
        assert_eq!(occurred_at.len(), 20);
        assert!(occurred_at.ends_with('Z'));
    }

    /// Mirrors `AgentAttentionSettingsTests.webhookCompatibilityEnvelope`.
    #[test]
    fn attention_events_carry_the_same_notification_buckets_as_the_app() {
        let notifier = Notifier::new(&enabled_settings());
        let inner = notifier.inner.as_ref().expect("enabled");

        let approval = inner.payload(
            NotificationEvent::AttentionRequired,
            status_label(RuntimeStatus::WaitingApproval),
            None,
            Some("tool_approval".to_owned()),
            Some("git status".to_owned()),
        );
        assert_eq!(approval.status, "pendingApproval");
        assert_eq!(approval.notification_type, Some("permission_prompt"));
        assert_eq!(approval.hook_event_name, "Notification");
        assert_eq!(approval.r#type, "approval-requested");
        // `message` mirrors `summary` on attention events.
        assert_eq!(approval.message.as_deref(), Some("git status"));

        let question = inner.payload(
            NotificationEvent::AttentionRequired,
            status_label(RuntimeStatus::WaitingAnswer),
            None,
            Some("ask_user".to_owned()),
            Some("请选择下一步".to_owned()),
        );
        assert_eq!(question.status, "awaitingUserAnswer");
        assert_eq!(question.notification_type, Some("elicitation_dialog"));

        let blocked = inner.payload(
            NotificationEvent::AttentionRequired,
            status_label(RuntimeStatus::Blocked),
            None,
            None,
            None,
        );
        assert_eq!(blocked.notification_type, Some("idle_prompt"));
    }

    #[test]
    fn summary_is_truncated_and_never_carries_a_transcript() {
        let notifier = Notifier::new(&enabled_settings());
        let inner = notifier.inner.as_ref().expect("enabled");
        let payload = inner.payload(
            NotificationEvent::AttentionRequired,
            status_label(RuntimeStatus::WaitingApproval),
            None,
            None,
            Some("x".repeat(MAX_SUMMARY_CHARS + 50)),
        );

        let summary = payload.summary.as_deref().expect("summary");
        assert_eq!(summary.chars().count(), MAX_SUMMARY_CHARS + 1);
        assert!(summary.ends_with('…'));
        // `message` is the same clamped text, not the raw input.
        assert_eq!(payload.message.as_deref(), Some(summary));
    }

    #[test]
    fn delivery_without_a_runtime_records_an_error_instead_of_panicking() {
        let notifier = Notifier::new(&enabled_settings());
        notifier.task_completed("Turn finished");
        let error = notifier.take_error().expect("error recorded");
        assert!(error.contains("no async runtime"));
        assert!(notifier.take_error().is_none());
    }

    #[tokio::test]
    async fn posts_a_real_request_to_a_local_listener() {
        use std::sync::mpsc;

        type Capture = (axum::http::HeaderMap, serde_json::Value);
        let (tx, rx) = mpsc::channel::<Capture>();
        let tx = Arc::new(Mutex::new(tx));
        let app = axum::Router::new().route(
            "/willdeep",
            axum::routing::post(
                move |headers: axum::http::HeaderMap, body: axum::Json<serde_json::Value>| {
                    let tx = tx.clone();
                    async move {
                        if let Ok(tx) = tx.lock() {
                            let _ = tx.send((headers, body.0));
                        }
                        axum::http::StatusCode::NO_CONTENT
                    }
                },
            ),
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
        let notifier = Notifier::new(&settings);
        notifier.set_session("runtime-42", Some("Approval run"));
        notifier.attention_required(
            RuntimeStatus::WaitingApproval,
            "tool_approval",
            "run_command: rm -rf build",
        );

        let (headers, body) =
            tokio::task::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(10)))
                .await
                .expect("join")
                .expect("webhook delivered");

        // Headers the app's receiver routes on before it parses the body.
        assert_eq!(headers["x-agent-source"], "willdeep");
        assert_eq!(headers["x-agent-executor"], "willdeep");
        assert_eq!(headers["x-webhook-schema"], "willdeep.webhook.v1");
        assert_eq!(headers["x-agent-event"], "attention_required");
        assert_eq!(headers["x-app-version"], APP_VERSION);
        assert_eq!(headers["content-type"], "application/json");
        let user_agent = headers["user-agent"].to_str().expect("user agent");
        assert!(user_agent.contains("WillDeep-"), "got {user_agent}");

        assert_eq!(body["schema_version"], "willdeep.webhook.v1");
        assert_eq!(body["event"], "attention_required");
        assert_eq!(body["notification_type"], "permission_prompt");
        assert_eq!(body["status"], "pendingApproval");
        assert_eq!(body["attention_kind"], "tool_approval");
        assert_eq!(body["session_id"], "runtime-42");
        assert_eq!(body["thread-id"], "runtime-42");
        assert_eq!(body["message"], "run_command: rm -rf build");
        assert!(notifier.take_error().is_none(), "a 2xx is not an error");
    }

    #[tokio::test]
    async fn failed_delivery_surfaces_an_error_without_breaking_the_caller() {
        let mut settings = enabled_settings();
        // Reserved TEST-NET-1 address: routable nowhere, so this always fails.
        settings.webhook_url = Some("http://192.0.2.1:9/willdeep".to_owned());
        let notifier = Notifier::new(&settings);
        notifier.task_completed("Turn finished");

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
