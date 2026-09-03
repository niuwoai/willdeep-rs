//! `willdeep event`：看事件内核里有什么，以及把一条标掉。
//!
//! 这是**观察面，不是权限中心**。审批类事件的唯一决策源仍是原来的审批卡片；
//! 在这里 `ignore` 一行只是把它从待办里划掉，不代表批准了任何操作。终端、
//! 文件、浏览器和 MCP 仍走各自原有的授权路径。

use anyhow::{Context, Result};
use clap::Subcommand;
use willdeep_core::kernel_store::KernelStore;
use willdeep_runtime_protocol::kernel_event::{DeliveryState, KernelEvent};

#[derive(Clone, Debug, Subcommand)]
pub(crate) enum EventAction {
    /// List runtime events, newest first.
    List {
        /// Only this session.
        #[arg(long)]
        session: Option<uuid::Uuid>,
        /// Only events still waiting on a person.
        #[arg(long)]
        pending: bool,
        /// Emit one JSON array instead of a table.
        #[arg(long)]
        json: bool,
        /// Maximum rows.
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
    /// Show one event in full.
    Show {
        /// Event ID, as printed by `event list`.
        id: uuid::Uuid,
    },
    /// Mark one event as no longer needing a person.
    ///
    /// This settles the user side only. It never approves anything: an
    /// approval still has to be answered where it was asked.
    Ignore {
        /// Event ID, as printed by `event list`.
        id: uuid::Uuid,
    },
}

pub(crate) fn run(action: EventAction, home: &std::path::Path) -> Result<()> {
    let store = KernelStore::new(home);
    match action {
        EventAction::List {
            session,
            pending,
            json,
            limit,
        } => list(&store, session, pending, json, limit),
        EventAction::Show { id } => show(&store, id),
        EventAction::Ignore { id } => ignore(&store, id),
    }
}

/// 读盘而不是连内核：`willdeep event` 是个一次性进程，那个跑着 Agent 的进程
/// 的内存它够不着。日志就是两者之间的共享事实。
fn load(store: &KernelStore) -> Vec<(uuid::Uuid, Vec<KernelEvent>)> {
    let report = store.load_all();
    for (path, reason) in &report.quarantined {
        eprintln!(
            "willdeep: quarantined a damaged event log ({reason}); kept at {}",
            path.display()
        );
    }
    report.sessions
}

fn list(
    store: &KernelStore,
    session: Option<uuid::Uuid>,
    pending: bool,
    json: bool,
    limit: usize,
) -> Result<()> {
    let mut events: Vec<KernelEvent> = load(store)
        .into_iter()
        .filter(|(id, _)| session.is_none_or(|wanted| *id == wanted))
        .flat_map(|(_, events)| events)
        .filter(|event| {
            !pending
                || (event.requires_user_action
                    && !matches!(
                        event.delivery.state,
                        DeliveryState::Ignored | DeliveryState::Resolved
                    ))
        })
        .collect();
    events.sort_by(|left, right| right.created_at.cmp(&left.created_at));
    events.truncate(limit);

    if json {
        println!("{}", serde_json::to_string_pretty(&events)?);
        return Ok(());
    }
    if events.is_empty() {
        println!("no runtime events");
        return Ok(());
    }
    for event in &events {
        // 合并次数与「谁还欠这条事件一个动作」是这张表最该回答的两件事，
        // 所以它们跟标题在同一行，而不是藏进详情。
        let merged = if event.merge_count > 1 {
            format!(" x{}", event.merge_count)
        } else {
            String::new()
        };
        println!(
            "{}  {:<10} {:<9} {:<12} {}{}",
            event.event_id,
            format!("{:?}", event.source).to_lowercase(),
            format!("{:?}", event.priority).to_lowercase(),
            delivery_label(event),
            event.title,
            merged
        );
    }
    Ok(())
}

fn delivery_label(event: &KernelEvent) -> String {
    let model = match event.delivery.state {
        DeliveryState::Handled => "seen",
        DeliveryState::Leased => "sending",
        DeliveryState::Ignored => "ignored",
        DeliveryState::Resolved => "resolved",
        DeliveryState::Pending => "queued",
    };
    // 「模型已经看过」不代表「用户不用管了」。两件事分开写，别让一个词
    // 同时承担两种含义。
    if event.requires_user_action
        && !matches!(
            event.delivery.state,
            DeliveryState::Ignored | DeliveryState::Resolved
        )
    {
        format!("{model}/you")
    } else {
        model.to_owned()
    }
}

fn show(store: &KernelStore, id: uuid::Uuid) -> Result<()> {
    let event = find(store, id)?;
    println!("{}", serde_json::to_string_pretty(&event)?);
    Ok(())
}

fn ignore(store: &KernelStore, id: uuid::Uuid) -> Result<()> {
    let session = load(store)
        .into_iter()
        .find(|(_, events)| events.iter().any(|event| event.event_id == id))
        .map(|(session, _)| session)
        .with_context(|| format!("no runtime event {id}"))?;
    let mut events = store
        .load_session(session)
        .map_err(anyhow::Error::msg)
        .with_context(|| format!("read the event log for session {session}"))?;
    let Some(event) = events.iter_mut().find(|event| event.event_id == id) else {
        anyhow::bail!("no runtime event {id}");
    };
    if !event.requires_user_action {
        println!("{id} was not waiting on you; nothing to do");
        return Ok(());
    }
    event.delivery.state = DeliveryState::Ignored;
    event.requires_user_action = false;
    store
        .save_session(session, &events)
        .with_context(|| format!("write the event log for session {session}"))?;
    println!("{id} ignored");
    // 说清楚这一步做了什么、没做什么：这条命令最容易被当成「批准」。
    println!(
        "note: this settles your side only; approvals are still answered where they were asked"
    );
    Ok(())
}

fn find(store: &KernelStore, id: uuid::Uuid) -> Result<KernelEvent> {
    load(store)
        .into_iter()
        .flat_map(|(_, events)| events)
        .find(|event| event.event_id == id)
        .with_context(|| format!("no runtime event {id}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use willdeep_core::kernel::{DedupPolicy, EventKernel, InterruptPolicy, host_event};
    use willdeep_runtime_protocol::kernel_event::{EventPriority, EventSource};

    fn seeded() -> (KernelStore, uuid::Uuid, uuid::Uuid) {
        let home =
            std::env::temp_dir().join(format!("willdeep-event-cmd-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).expect("home");
        let store = KernelStore::new(&home);
        let session = uuid::Uuid::new_v4();
        let kernel = EventKernel::new();
        let mut event = host_event(
            session,
            EventSource::External,
            "external.notice",
            EventPriority::Normal,
            InterruptPolicy::YieldAtBoundary,
            "build 1841 failed",
            Some("log tail".to_owned()),
            Some("ingress:1841".to_owned()),
            true,
        );
        event.audience.user = true;
        let id = event.event_id;
        kernel.publish(event, DedupPolicy::Once);
        willdeep_core::kernel_store::flush(&kernel, &store);
        (store, session, id)
    }

    #[test]
    fn ignoring_settles_the_user_side_and_persists() {
        let (store, session, id) = seeded();
        ignore(&store, id).expect("ignore");
        let stored = store.load_session(session).expect("load");
        let event = stored
            .iter()
            .find(|event| event.event_id == id)
            .expect("still stored");
        assert_eq!(event.delivery.state, DeliveryState::Ignored);
        assert!(!event.requires_user_action);
        // 事件本身留着：忽略是「我看过了」，不是「这件事没发生过」。
        assert_eq!(stored.len(), 1);
    }

    #[test]
    fn unknown_ids_are_reported_not_silently_ignored() {
        let (store, _, _) = seeded();
        let error = ignore(&store, uuid::Uuid::new_v4()).expect_err("unknown id");
        assert!(error.to_string().contains("no runtime event"));
    }

    /// 「模型看过」与「还要人处理」在列表里必须分得开。
    #[test]
    fn delivery_label_keeps_the_two_lanes_apart() {
        let (store, session, id) = seeded();
        let mut events = store.load_session(session).expect("load");
        let event = events
            .iter_mut()
            .find(|event| event.event_id == id)
            .expect("event");
        event.delivery.state = DeliveryState::Handled;
        assert_eq!(delivery_label(event), "seen/you");
        event.requires_user_action = false;
        assert_eq!(delivery_label(event), "seen");
    }
}
