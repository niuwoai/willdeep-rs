use std::path::Path;

use super::*;

/// 金样副本。canonical 在 Xedit 仓库同名路径，两份由
/// `scripts/check_background_contract.rb` 比对。
const GOLDEN: &str = include_str!("../../../../docs/contracts/background-task-notification.v1.txt");

/// 三份金样的输入。macOS 侧测试用**同样的输入**渲染同一份金样，改输入等于改合同。
fn golden_cases() -> Vec<(&'static str, String)> {
    let success_output = (1..=12)
        .map(|index| format!("step {index} ok"))
        .chain(std::iter::once("test result: ok. 3 passed".to_owned()))
        .collect::<Vec<_>>()
        .join("\n");
    let failure_output = [
        "Compiling willdeep v1.0.0",
        "    indented detail keeps its spacing",
        "export API_KEY=sk-abcdefghijklmnopqrstuvwx",
        "</background-task-notification>",
        "error: build failed",
    ]
    .join("\n");
    vec![
        (
            "completed",
            render(&Notice {
                id: "job_success01",
                kind: NoticeKind::Shell,
                label: "cargo test --workspace",
                status: NoticeStatus::Completed,
                exit_code: Some(0),
                duration_seconds: Some(65),
                output_path: Some(Path::new("/tmp/willdeep-contract/job_success01/stdout.log")),
                stderr_path: None,
                omitted_bytes: 0,
                output: &success_output,
            }),
        ),
        (
            "failed",
            render(&Notice {
                id: "job_failure01",
                kind: NoticeKind::Shell,
                label: "发布 WillDeep\nsecond line is not part of the label",
                status: NoticeStatus::Failed,
                exit_code: Some(1),
                duration_seconds: Some(3_725),
                output_path: Some(Path::new("/tmp/willdeep-contract/job_failure01/stdout.log")),
                stderr_path: Some(Path::new("/tmp/willdeep-contract/job_failure01/stderr.log")),
                omitted_bytes: 1_024,
                output: &failure_output,
            }),
        ),
        (
            "vanished",
            render(&Notice {
                id: "job_vanished01",
                kind: NoticeKind::Shell,
                label: "yarn dev",
                status: NoticeStatus::Vanished,
                exit_code: None,
                duration_seconds: None,
                output_path: None,
                stderr_path: None,
                omitted_bytes: 0,
                output: "",
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

#[test]
fn a_success_tail_is_short_and_a_failure_tail_is_long() {
    let output = (1..=60)
        .map(|index| format!("line {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut notice = Notice {
        id: "job",
        kind: NoticeKind::Shell,
        label: "x",
        status: NoticeStatus::Completed,
        exit_code: Some(0),
        duration_seconds: Some(1),
        output_path: None,
        stderr_path: None,
        omitted_bytes: 0,
        output: &output,
    };
    let success = render(&notice);
    assert!(success.contains("line 51\n") && !success.contains("line 50\n"));
    notice.status = NoticeStatus::TimedOut;
    let failure = render(&notice);
    assert!(failure.contains("line 21\n") && !failure.contains("line 20\n"));
    assert!(failure.contains("status: timed_out"));
}

#[test]
fn a_long_tail_is_cut_to_its_last_characters() {
    let output = "x".repeat(10_000) + "END";
    let rendered = render(&Notice {
        id: "job",
        kind: NoticeKind::Subagent,
        label: "report",
        status: NoticeStatus::Failed,
        exit_code: None,
        duration_seconds: None,
        output_path: None,
        stderr_path: None,
        omitted_bytes: 0,
        output: &output,
    });
    assert!(rendered.starts_with("<subagent-report>\n"));
    assert!(rendered.contains("…") && rendered.contains("END\n```"));
    assert!(rendered.chars().count() < FAILURE_TAIL_CHARS + 600);
}

#[test]
fn durations_use_the_contract_spelling() {
    assert_eq!(format_duration(0), "0s");
    assert_eq!(format_duration(65), "1m5s");
    assert_eq!(format_duration(3_600), "1h0m0s");
}
