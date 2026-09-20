//! 创建会话、提交一轮、跟到结束。
//!
//! ```bash
//! willdeep daemon start
//! cargo run -p willdeep-runtime-client --example submit_turn -- /path/to/workspace "总结这个项目的风险"
//! ```
//!
//! 事件游标在提交前先记下（`runtime.status` 的 `event_sequence`），一条也不漏；
//! `task.output` 里整段的 `assistant_text` 直接打印；看到这一轮的 `turn.*` 终态事件就停。
//! 流被服务端关掉（比如 `willdeep daemon upgrade` 排空交接）就重读 `daemon.json`、
//! 沿最后一个序号重连——事件日志是持久的，中间完成的轮次不会丢。

mod common;

use willdeep_runtime_protocol::{CreateSessionParams, RuntimeEvent, SubmitTurnParams};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: submit_turn <workspace> <prompt…>";
    let workspace = args.next().ok_or(usage)?;
    let prompt = args.collect::<Vec<_>>().join(" ");
    if prompt.trim().is_empty() {
        return Err(usage.into());
    }
    let workspace = std::fs::canonicalize(&workspace)?.display().to_string();

    let mut client = common::connect()?;
    let capabilities = common::unwrap(client.capabilities(None).await?)?;
    if !capabilities
        .operations
        .iter()
        .any(|operation| operation == "turn.submit")
    {
        return Err(format!(
            "this Runtime (protocol {}) does not serve turn.submit",
            capabilities.protocol_version
        )
        .into());
    }
    let mut after = common::unwrap(client.status().await?)?.event_sequence;

    let session = common::unwrap(
        client
            .create_session(
                &CreateSessionParams {
                    id: None,
                    workspace,
                    profile: None,
                    model: None,
                    title: Some("SDK example".to_owned()),
                },
                uuid::Uuid::new_v4(),
            )
            .await?,
    )?;
    let turn = common::unwrap(
        client
            .submit_turn(
                &SubmitTurnParams {
                    session_id: session.id,
                    turn_request_id: uuid::Uuid::new_v4(),
                    prompt,
                    attachments: Vec::new(),
                    origin_client: Some("sdk-example".to_owned()),
                },
                uuid::Uuid::new_v4(),
            )
            .await?,
    )?;
    println!(
        "session {} · turn {} · following events after #{after}",
        session.id, turn.id
    );

    let terminal_marker = format!("turn_id={}", turn.id);
    loop {
        let mut stream = client.stream_events(after, 200, None).await?;
        while let Some(envelope) = stream.next::<RuntimeEvent>().await? {
            let event = common::unwrap(envelope)?;
            after = event.sequence;
            if event.kind == "task.output" {
                if let Some(text) = assistant_text(&event.message) {
                    println!("{text}");
                }
            } else if event.kind.starts_with("turn.") && event.message.contains(&terminal_marker) {
                println!("{}: {}", event.kind, event.message);
                return Ok(());
            }
        }
        eprintln!("event stream closed after #{after}; reconnecting");
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        client = common::connect()?;
    }
}

/// `task.output` 的 message 是 `task_id=<uuid> {json}`；只挑整段的 `assistant_text`，
/// 逐字增量（`assistant_text_delta`）留给要做打字机效果的界面。
fn assistant_text(message: &str) -> Option<String> {
    let json = &message[message.find('{')?..];
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    (value["type"] == "assistant_text")
        .then(|| value["text"].as_str().unwrap_or_default().to_owned())
}
