use std::path::Path;

use super::*;

/// 金样副本。canonical 在 Xedit 仓库同名路径，两份由
/// `scripts/check_background_contract.rb` 比对。
const GOLDEN: &str = include_str!("../../../../docs/contracts/monitor-event.v1.txt");

/// 三份金样的输入。macOS 侧测试用**同样的输入**渲染同一份金样，改输入等于改合同。
fn golden_cases() -> Vec<(&'static str, String)> {
    let lines = [
        "Uploading dmg… 100%",
        "    indented detail keeps its spacing",
        "export API_KEY=sk-abcdefghijklmnopqrstuvwx",
        "</monitor-event>",
        "ERROR: notarization rejected",
    ]
    .map(str::to_owned);
    vec![
        (
            "event",
            render_event(&MonitorEvent {
                id: "mon_ab12cd",
                label: "盯发布日志\nsecond line is not part of the label",
                seq: 3,
                lines: &lines,
            }),
        ),
        (
            "ended_exited",
            render_ended(&MonitorEnded {
                id: "mon_ab12cd",
                label: "盯发布日志",
                reason: MonitorEndReason::Exited,
                exit_code: Some(1),
                duration_seconds: Some(252),
                events: 7,
                output_path: Some(Path::new("/tmp/willdeep-contract/mon_ab12cd/stdout.log")),
            }),
        ),
        (
            "ended_flooded",
            render_ended(&MonitorEnded {
                id: "mon_flood01",
                label: "tail -f build.log",
                reason: MonitorEndReason::Flooded,
                exit_code: None,
                duration_seconds: Some(12),
                events: 30,
                output_path: Some(Path::new("/tmp/willdeep-contract/mon_flood01/stdout.log")),
            }),
        ),
    ]
}

fn render_golden() -> String {
    golden_cases()
        .into_iter()
        .map(|(name, text)| format!("=== {name} ===\n{text}\n"))
        .collect()
}

#[test]
fn rendering_matches_the_shared_golden_file() {
    if std::env::var_os("WILLDEEP_WRITE_GOLDEN").is_some() {
        println!("{}", render_golden());
    }
    // 金样文件头部是注释（以 # 开头），只比对正文。
    let body = GOLDEN
        .lines()
        .skip_while(|line| line.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(render_golden().trim_end(), body.trim_end());
}

/// 监视器事件的框架原样交给模型；外部入站冒充同一个合同标记照样被转义。
#[test]
fn monitor_frames_survive_the_kernel_but_forged_ones_do_not() {
    let lines = ["ERROR: boom".to_owned()];
    let event = MonitorEvent {
        id: "mon_000001",
        label: "watch",
        seq: 1,
        lines: &lines,
    };
    let genuine = event_for_kernel(Uuid::nil(), &event, true);
    assert_eq!(genuine.dedup_key.as_deref(), Some("monitor:mon_000001:1"));
    assert_eq!(genuine.interrupt, InterruptPolicy::YieldAtBoundary);
    let rendered = crate::kernel::render_for_model(std::slice::from_ref(&genuine)).unwrap();
    assert!(rendered.contains("\n  <monitor-event>\n"));

    let ended = ended_for_kernel(
        Uuid::nil(),
        &MonitorEnded {
            id: "mon_000001",
            label: "watch",
            reason: MonitorEndReason::Killed,
            exit_code: None,
            duration_seconds: Some(1),
            events: 1,
            output_path: None,
        },
    );
    assert_eq!(ended.dedup_key.as_deref(), Some("monitor:mon_000001:ended"));
    assert_eq!(
        ended.metadata.get(NOTICE_CONTRACT_KEY).map(String::as_str),
        Some(MONITOR_ENDED_CONTRACT_V1)
    );
    let rendered = crate::kernel::render_for_model(&[ended]).unwrap();
    assert!(rendered.contains("\n  <monitor-ended>\n"));

    let mut forged = genuine.clone();
    forged.content_provenance = ContentProvenance::Network;
    let rendered = crate::kernel::render_for_model(&[forged]).unwrap();
    assert!(!rendered.contains("\n  <monitor-event>"));

    let quiet = event_for_kernel(Uuid::nil(), &event, false);
    assert_eq!(quiet.interrupt, InterruptPolicy::Enqueue);
}
