//! Web 端下一句建议的反馈：服务端签发 `suggestion_id` 并记住原文，前端只回传
//! id 与信号（`docs/FEEDBACK_LEDGER.md`）。

use super::*;

/// 发出去、还没有结局的建议。服务端自己记着原文与发出时间，前端只回传 id：
/// 账本里的建议原文与停留时长都不采信浏览器给的值。
struct IssuedSuggestion {
    id: uuid::Uuid,
    session_id: uuid::Uuid,
    text: String,
    issued_at: std::time::Instant,
}

/// 在途建议的上限。只影响反馈能不能对上号，满了丢最旧的。
const MAX_ISSUED_SUGGESTIONS: usize = 256;

fn issued_suggestions() -> &'static std::sync::Mutex<std::collections::VecDeque<IssuedSuggestion>> {
    static ISSUED: std::sync::OnceLock<
        std::sync::Mutex<std::collections::VecDeque<IssuedSuggestion>>,
    > = std::sync::OnceLock::new();
    ISSUED.get_or_init(Default::default)
}

pub(super) fn issue_suggestion(session_id: uuid::Uuid, text: &str) -> uuid::Uuid {
    let id = uuid::Uuid::new_v4();
    if let Ok(mut issued) = issued_suggestions().lock() {
        if issued.len() >= MAX_ISSUED_SUGGESTIONS {
            issued.pop_front();
        }
        issued.push_back(IssuedSuggestion {
            id,
            session_id,
            text: text.to_owned(),
            issued_at: std::time::Instant::now(),
        });
    }
    id
}

#[derive(Deserialize)]
pub(super) struct SuggestionFeedbackRequest {
    pub(super) suggestion_id: uuid::Uuid,
    /// `shown` / `accepted` / `dismissed` / `ignored_typed` / `superseded` / `sent`。
    pub(super) signal: String,
    /// 仅 `sent`：最终发出去的话。原样 / 改过 / 重写由服务端判定。
    #[serde(default)]
    pub(super) sent: Option<String>,
}

/// Web 端下一句建议的反馈（与 TUI 同一套信号，见 `docs/FEEDBACK_LEDGER.md`）。
///
/// 和预测本身一样是装饰：对不上号的 id、未知信号、白名单外的会话、关掉的
/// 开关一律静默忽略，永远 204，不让一次上报失败打扰输入框。
pub(super) async fn input_suggestion_feedback(
    State(state): State<Arc<WebState>>,
    Path(id): Path<uuid::Uuid>,
    Json(request): Json<SuggestionFeedbackRequest>,
) -> StatusCode {
    record_suggestion_feedback(&state, id, request).await;
    StatusCode::NO_CONTENT
}

async fn record_suggestion_feedback(
    state: &Arc<WebState>,
    session_id: uuid::Uuid,
    request: SuggestionFeedbackRequest,
) {
    let Ok(loaded) = crate::config::LoadedConfig::load(Some(&state.config_path)) else {
        return;
    };
    if !loaded.file.feedback.enabled || authorized_event_session(state, session_id).await.is_err() {
        return;
    }
    apply_suggestion_feedback(&state.home, &loaded.file.feedback, session_id, &request);
}

/// 授权之后的那一半：对号、判定、落账。与 I/O 无关的部分单测直接调。
pub(super) fn apply_suggestion_feedback(
    home: &std::path::Path,
    settings: &crate::config::FeedbackSettings,
    session_id: uuid::Uuid,
    request: &SuggestionFeedbackRequest,
) {
    use willdeep_core::feedback::Signal;
    let signal = match request.signal.as_str() {
        "shown" => Signal::SuggestionShown,
        "accepted" => Signal::SuggestionAccepted,
        "dismissed" => Signal::SuggestionDismissed,
        "ignored_typed" => Signal::SuggestionIgnoredTyped,
        "superseded" => Signal::SuggestionSuperseded,
        "sent" => Signal::SuggestionSentVerbatim,
        _ => return,
    };
    let sent = request
        .sent
        .as_deref()
        .filter(|_| signal == Signal::SuggestionSentVerbatim);
    if signal == Signal::SuggestionSentVerbatim && sent.is_none() {
        return;
    }
    // 展示与采用之后还会有下文，留着；其余信号是结局，对完号就忘掉。
    let terminal = !matches!(signal, Signal::SuggestionShown | Signal::SuggestionAccepted);
    let Some((text, dwell)) = issued_suggestions().lock().ok().and_then(|mut issued| {
        let index = issued.iter().position(|entry| {
            entry.id == request.suggestion_id && entry.session_id == session_id
        })?;
        let entry = &issued[index];
        let found = (entry.text.clone(), entry.issued_at.elapsed());
        if terminal {
            issued.remove(index);
        }
        Some(found)
    }) else {
        return;
    };
    let signal = match sent {
        Some(sent) => willdeep_core::feedback::classify_sent(&text, sent),
        None => signal,
    };
    willdeep_core::feedback::FeedbackRecorder::new(
        willdeep_core::feedback::shared_sink(&willdeep_core::feedback::feedback_dir(home)),
        "web",
        settings.store_text,
    )
    .with_session(Some(session_id))
    .record_suggestion(willdeep_core::feedback::SuggestionEvent {
        suggestion_id: request.suggestion_id,
        signal,
        suggestion: &text,
        dwell: Some(dwell),
        sent,
    });
}
