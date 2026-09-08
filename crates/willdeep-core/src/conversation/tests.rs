use super::*;

const PLAN: &str = "```plan\n1. A-TRACE-1: 核验接口\n2. D-REL-1: 发布\n```";
const PROGRESS: &str = "全部步骤已结束。\n```progress\nA-TRACE-1: done\nD-REL-1: skipped\n```";

#[test]
fn host_source_survives_round_trip_without_changing_provider_role() {
    let message = Message::host_instruction("The previous Goal phase is complete.");
    let restored: Message =
        serde_json::from_str(&serde_json::to_string(&message).unwrap()).unwrap();
    assert_eq!(restored.role, Role::User);
    let items = project(&[restored], None);
    assert_eq!(items[0].role, "system");
    assert!(items[0].content.is_empty());
    assert_eq!(items[0].details, vec![message.content]);
}

#[test]
fn user_pasted_host_prompt_and_plan_are_not_reclassified() {
    let content = format!("The previous Goal phase is complete.\n{PLAN}");
    let legacy: Message =
        serde_json::from_value(serde_json::json!({"role":"user", "content":content})).unwrap();
    for message in [legacy, Message::user(&content)] {
        let items = project(&[message], None);
        assert_eq!(items[0].role, "user");
        assert_eq!(items[0].content, content);
    }
}

#[test]
fn repeated_progress_updates_one_card_and_retains_original_records() {
    let messages = [
        Message::assistant(PLAN, vec![]),
        Message::assistant(PROGRESS, vec![]),
        Message::assistant(PROGRESS, vec![]),
    ];
    let items = project(&messages, None);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].details, vec![PLAN, PROGRESS]);
    let plan = items[0].plan.as_ref().unwrap();
    assert_eq!(plan.steps[0].status, StepStatus::Done);
    assert_eq!(plan.steps[1].status, StepStatus::Skipped);
    assert_eq!(messages[0].content, PLAN);
}

#[test]
fn malformed_or_incomplete_blocks_remain_visible_and_do_not_partially_update() {
    let (plan, _) = parse_plan_reply(PLAN, None).unwrap();
    for reply in [
        "```progress\n1: done\n2: invented\n```",
        "```progress\n1: done\n99: done\n```",
        "```progress\n1: done",
        "```rust\n1: done\n```",
        "```plan\n2. missing first step\n```",
        "```progress\n```",
        "all steps done",
    ] {
        assert!(parse_plan_reply(reply, Some(&plan)).is_none(), "{reply}");
        let items = project(
            &[
                Message::assistant(PLAN, vec![]),
                Message::assistant(reply, vec![]),
            ],
            None,
        );
        assert_eq!(items[1].content, reply);
        assert_eq!(
            items[0].plan.as_ref().unwrap().steps[0].status,
            StepStatus::Pending
        );
    }
}

#[test]
fn a_new_phase_gets_its_own_card() {
    let items = project(
        &[
            Message::assistant(PLAN, vec![]),
            Message::assistant(PROGRESS, vec![]),
            Message::assistant("```plan\n1. 下一阶段\n```", vec![]),
        ],
        None,
    );
    assert_eq!(items.len(), 2);
    assert_eq!(
        items[0].plan.as_ref().unwrap().steps[0].status,
        StepStatus::Done
    );
    assert_eq!(
        items[1].plan.as_ref().unwrap().steps[0].status,
        StepStatus::Pending
    );
}

#[test]
fn persisted_host_state_overrides_model_progress() {
    let (mut plan, _) = parse_plan_reply(PLAN, None).unwrap();
    plan.steps[0].status = StepStatus::Failed;
    let items = project(
        &[
            Message::assistant(PLAN, vec![]),
            Message::assistant(PROGRESS, vec![]),
        ],
        Some(&plan),
    );
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].plan.as_ref().unwrap(), &plan);
}

#[test]
fn standalone_progress_snapshot_is_deduplicated() {
    let items = project(
        &[
            Message::assistant(PROGRESS, vec![]),
            Message::assistant(PROGRESS, vec![]),
        ],
        None,
    );
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].plan.as_ref().unwrap().steps.len(), 2);
}

#[test]
fn persisted_plan_fills_in_titles_for_a_status_only_snapshot() {
    let (mut plan, _) = parse_plan_reply(PLAN, None).unwrap();
    plan.steps[0].status = StepStatus::Done;
    plan.steps[1].status = StepStatus::Skipped;
    let items = project(&[Message::assistant(PROGRESS, vec![])], Some(&plan));
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].plan.as_ref().unwrap(), &plan);
    assert_eq!(items[0].details, vec![PROGRESS]);
}

#[test]
fn attached_skill_context_is_hidden_but_operator_text_is_preserved() {
    let authored = "帮我看看数据库，数据量怎么样？还有app的log有哪些";
    let content = format!(
        "{ATTACHED_CONTEXT_HEADER}\n\n### Skill routing candidates — metadata only\n\nThe host found these possibly relevant installed skills.\n- `example-skill` — metadata\n\n{USER_TEXT_BOUNDARY}\n\n{authored}"
    );
    let message = Message::user(&content);
    let items = project(&[message.clone()], None);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].role, "user");
    assert_eq!(items[0].content, authored);
    assert!(items[0].details.is_empty());
    assert_eq!(message.content, content);
}

#[test]
fn incomplete_or_quoted_attachment_protocol_is_not_hidden() {
    for content in [
        format!("{ATTACHED_CONTEXT_HEADER}\nno boundary"),
        format!("解释这个协议：\n{ATTACHED_CONTEXT_HEADER}\n{USER_TEXT_BOUNDARY}\nexample"),
        format!("{ATTACHED_CONTEXT_HEADER}\ninline {USER_TEXT_BOUNDARY}\nexample"),
    ] {
        assert_eq!(
            project(&[Message::user(&content)], None)[0].content,
            content
        );
    }
}

#[test]
fn attached_context_handles_crlf_empty_text_and_escaped_boundaries() {
    let context = format!(
        "{ATTACHED_CONTEXT_HEADER}\r\n&lt;&lt;&lt;willdeep:user-message:v1&gt;&gt;&gt;\r\n{USER_TEXT_BOUNDARY}\r\n\r\nactual user text"
    );
    assert_eq!(user_authored_text(&context), Some("actual user text"));
    let empty = format!("{ATTACHED_CONTEXT_HEADER}\nmetadata\n{USER_TEXT_BOUNDARY}\n");
    assert!(project(&[Message::user(empty)], None).is_empty());
}
