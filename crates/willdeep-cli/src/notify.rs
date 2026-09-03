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
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use willdeep_core::{
    AttentionItem, AttentionSection, AttentionSource, RuntimeStatus, format_iso8601,
};

use crate::config::NotificationSettings;

/// Matches `URLSessionConfiguration.timeoutIntervalForRequest` on the app side
/// so a slow endpoint behaves the same whichever client is talking to it.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Ceiling on how long we wait for the TCP connection itself.
///
/// Must stay below `FLUSH_TIMEOUT`, otherwise an endpoint that accepts nothing
/// and answers nothing — a black-holed address, a host that dropped off the
/// network — burns the whole flush window without ever recording an error.
/// `REQUEST_TIMEOUT` alone cannot cover this: it is the ceiling for a request
/// that is already in flight, and a connect that never completes never gets
/// there. The failure it guards against is silent, which is why the delivery
/// error only shows up on some networks.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// Ceiling on how long `flush` will hold a process open waiting for deliveries
/// it already started. Shorter than `REQUEST_TIMEOUT`: a courtesy ping must
/// never become the reason `willdeep run` feels slow to exit.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(6);
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
    /// The gate keys present in the previous snapshot. Runtime attention is
    /// level-triggered — an open gate reappears in every snapshot — so this
    /// set is what converts it to edges: post on keys that just appeared, and
    /// forget a key once the gate closes so the *next* one fires again.
    open_gates: Mutex<HashSet<String>>,
    /// Deliveries already started. `flush` awaits these so a short-lived
    /// process does not exit out from under an in-flight request.
    pending: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// Delivery is detached, so failures land here instead of being swallowed;
    /// callers drain this and surface it.
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
        // One client name across every outgoing request. Vendor detection does
        // not rest on this string: the app's detector reads the headers first,
        // and `X-Agent-Source` / `X-Agent-Executor` below still carry the
        // lowercase `willdeep` the classifier matches on.
        let Ok(client) = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .user_agent(willdeep_core::CLIENT_USER_AGENT)
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
                open_gates: Mutex::new(HashSet::new()),
                pending: Mutex::new(Vec::new()),
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

    /// Level-triggered counterpart for runtime snapshots: post the gates that
    /// need a human, once per gate.
    ///
    /// "Once per gate" cannot mean "once per id". The daemon builds these with
    /// [`AttentionItem::approval`] / [`AttentionItem::question`], whose ids are
    /// the constants `approval:current` and `question:current` — remembering an
    /// id forever would post the first approval of a session and silently
    /// swallow every one after it. So the key carries the status and a content
    /// fingerprint too, and a key is forgotten as soon as the gate leaves the
    /// snapshot: closing one approval is what re-arms the next.
    pub fn attention_snapshot(&self, items: &[AttentionItem]) {
        let Some(inner) = &self.inner else { return };
        if !inner.on_attention_required {
            return;
        }
        let gates = items
            .iter()
            .filter(|item| item.status.section() == Some(AttentionSection::NeedsYou))
            .map(|item| (gate_key(item), item))
            .collect::<Vec<_>>();
        let current = gates
            .iter()
            .map(|(key, _)| key.clone())
            .collect::<HashSet<_>>();
        let Ok(mut open) = inner.open_gates.lock() else {
            return;
        };
        let opened = gates
            .into_iter()
            .filter(|(key, _)| !open.contains(key))
            .map(|(_, item)| item.clone())
            .collect::<Vec<_>>();
        *open = current;
        drop(open);
        for item in opened {
            self.post(inner.payload(
                NotificationEvent::AttentionRequired,
                status_label(item.status),
                Some(item.id.clone()),
                Some(source_label(item.source).to_owned()),
                Some(format!("{} · {}", item.title, item.detail)),
            ));
        }
    }

    /// Await deliveries already started, bounded by [`FLUSH_TIMEOUT`].
    ///
    /// A detached `tokio::spawn` is dropped when the runtime shuts down, so a
    /// short-lived process — `willdeep run` returns the moment the turn ends —
    /// would exit before the request ever left the socket. Long-lived frontends
    /// like the TUI never need this; the headless path always does.
    pub async fn flush(&self) {
        let Some(inner) = &self.inner else { return };
        let handles = match inner.pending.lock() {
            Ok(mut pending) => std::mem::take(&mut *pending),
            Err(_) => return,
        };
        if handles.is_empty() {
            return;
        }
        let joined = async {
            for handle in handles {
                let _ = handle.await;
            }
        };
        if tokio::time::timeout(FLUSH_TIMEOUT, joined).await.is_err() {
            inner.record_error("webhook delivery did not finish before exit".to_owned());
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
        let task = inner.clone();
        let delivery = handle.spawn(async move {
            let inner = task;
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
        if let Ok(mut pending) = inner.pending.lock() {
            // Drop handles that already finished so a long TUI session does not
            // accumulate one per notification.
            pending.retain(|handle| !handle.is_finished());
            pending.push(delivery);
        }
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

/// Which subsystem raised the gate.
///
/// Spelled out rather than derived from `Debug` or serde: this is a wire value,
/// and `{:?}` would silently change it the day an enum variant is renamed (it
/// also yields `diffreview`, which is neither the snake_case form nor anything
/// a receiver would expect).
///
/// Note this is not the same thing the desktop app puts in `attention_kind` —
/// `AgentToolApprovalNotifier` sends the *tool name* there. The CLI's approval
/// boundary only ever receives a localized human sentence ("run command: …"),
/// so a category is the most it can report honestly; inferring a tool name from
/// prose would break the moment the language changes.
fn source_label(source: AttentionSource) -> &'static str {
    match source {
        AttentionSource::Approval => "approval",
        AttentionSource::Question => "question",
        AttentionSource::BackgroundShell => "background_shell",
        AttentionSource::Subagent => "subagent",
        AttentionSource::Worktree => "worktree",
        AttentionSource::DiffReview => "diff_review",
    }
}

/// Identifies one *occurrence* of a gate, not one gate slot. The id alone is a
/// constant for daemon approvals and questions, so status and content have to
/// take part or consecutive approvals collapse into one notification.
fn gate_key(item: &AttentionItem) -> String {
    let mut hasher = DefaultHasher::new();
    item.title.hash(&mut hasher);
    item.detail.hash(&mut hasher);
    format!(
        "{}|{}|{:016x}",
        item.id,
        status_label(item.status),
        hasher.finish()
    )
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

    /// A loopback endpoint that records every body it receives, in order.
    /// Returns the port and the shared log.
    async fn capture_listener() -> (u16, Arc<Mutex<Vec<serde_json::Value>>>) {
        let received = Arc::new(Mutex::new(Vec::new()));
        let sink = received.clone();
        let app = axum::Router::new().route(
            "/willdeep",
            axum::routing::post(move |body: axum::Json<serde_json::Value>| {
                let sink = sink.clone();
                async move {
                    if let Ok(mut sink) = sink.lock() {
                        sink.push(body.0);
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
        (port, received)
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
    fn snapshot_tracks_open_gates_and_skips_non_gates() {
        let notifier = Notifier::new(&enabled_settings());
        let inner = notifier.inner.clone().expect("enabled");
        let mut working = needs_you("background:1");
        working.status = RuntimeStatus::Working;

        notifier.attention_snapshot(&[needs_you("diff-review:1"), working]);
        let open = inner.open_gates.lock().expect("lock").clone();
        assert_eq!(open.len(), 1, "only the NeedsYou item is a gate");
        assert!(open.iter().all(|key| key.starts_with("diff-review:1|")));

        // The gate closing must clear the key, or the next one cannot fire.
        notifier.attention_snapshot(&[]);
        assert!(inner.open_gates.lock().expect("lock").is_empty());
    }

    /// Regression: the daemon builds these with `AttentionItem::approval`, whose
    /// id is the constant `approval:current`. Keying de-duplication on the id
    /// alone posted the first approval of a session and silently swallowed every
    /// one after it.
    #[tokio::test]
    async fn consecutive_daemon_approvals_each_notify_despite_a_constant_id() {
        let (port, received) = capture_listener().await;
        let mut settings = enabled_settings();
        settings.webhook_url = Some(format!("http://127.0.0.1:{port}/willdeep"));
        let notifier = Notifier::new(&settings);

        let mut first = willdeep_core::AttentionItem::approval("run command: ls");
        first.source = AttentionSource::Approval;
        let mut second = willdeep_core::AttentionItem::approval("run command: rm -rf build");
        second.source = AttentionSource::Approval;
        assert_eq!(first.id, second.id, "the daemon reuses one id");

        notifier.attention_snapshot(std::slice::from_ref(&first));
        notifier.attention_snapshot(std::slice::from_ref(&first)); // still open
        notifier.attention_snapshot(&[]); // resolved
        notifier.attention_snapshot(std::slice::from_ref(&second)); // next approval
        notifier.flush().await;

        let bodies = received.lock().expect("lock").clone();
        let summaries = bodies
            .iter()
            .map(|body| body["summary"].as_str().unwrap_or_default().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            summaries.len(),
            2,
            "one post per approval, not per snapshot: {summaries:?}"
        );
        assert!(summaries[0].contains("run command: ls"));
        assert!(summaries[1].contains("run command: rm -rf build"));
        assert_eq!(bodies[0]["attention_kind"], "approval");
    }

    /// Regression: `willdeep run` returns straight into process teardown, so a
    /// detached delivery was dropped with the runtime and never left the socket.
    #[test]
    fn flush_lets_a_short_lived_process_finish_delivering() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let delivered = runtime.block_on(async {
            let (port, received) = capture_listener().await;
            let mut settings = enabled_settings();
            settings.webhook_url = Some(format!("http://127.0.0.1:{port}/willdeep"));
            let notifier = Notifier::new(&settings);
            notifier.set_session("runtime-42", None);
            notifier.task_completed("done");
            // Exactly what execute_noninteractive does before returning.
            notifier.flush().await;
            assert!(notifier.take_error().is_none());
            received.lock().expect("lock").len()
        });
        drop(runtime); // process exit
        assert_eq!(delivered, 1, "flush must not return before the POST lands");
    }

    #[test]
    fn source_labels_are_stable_wire_values_not_debug_output() {
        // `format!("{:?}", ..).to_lowercase()` used to produce "diffreview".
        assert_eq!(source_label(AttentionSource::DiffReview), "diff_review");
        assert_eq!(
            source_label(AttentionSource::BackgroundShell),
            "background_shell"
        );
        assert_eq!(source_label(AttentionSource::Approval), "approval");
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
        assert_eq!(headers["user-agent"], willdeep_core::CLIENT_USER_AGENT);

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
        // A closed port on loopback: the kernel refuses immediately, so the
        // failure path is exercised without a packet leaving the machine and
        // without depending on how this network treats unroutable addresses.
        // The old address here was TEST-NET-1, which is unroutable by
        // definition but not uniformly *fast* to fail: where the first hop
        // black-holes it instead of answering, the connect hangs until a
        // timeout the assertion window was shorter than, and this test failed
        // for reasons that had nothing to do with the notifier.
        settings.webhook_url = Some("http://127.0.0.1:1/willdeep".to_owned());
        let notifier = Notifier::new(&settings);
        notifier.task_completed("Turn finished");

        // Poll past the connect ceiling, so a slow refusal is still caught.
        let deadline = CONNECT_TIMEOUT + Duration::from_secs(2);
        let started = std::time::Instant::now();
        let mut error = None;
        while started.elapsed() < deadline {
            if let Some(found) = notifier.take_error() {
                error = Some(found);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            error.is_some_and(|value| value.contains("webhook delivery failed")),
            "expected a recorded delivery failure"
        );
    }
}
