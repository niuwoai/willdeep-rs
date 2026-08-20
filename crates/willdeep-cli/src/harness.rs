use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use willdeep_core::provider::{ApiDialect, ProviderConfig, ProviderKind};
use willdeep_core::tools::{ApprovalSource, ApprovalTrace};
use willdeep_core::{
    Agent, AgentConfig, ApprovalMode, Approver, BackgroundTaskKind, BackgroundTaskRegistry,
    BackgroundTaskStatus, EventSink, ProviderSafetyJudge, RoutingGuard, RoutingPolicy, SafetyJudge,
    SubagentCatalog, ToolRegistry, WebToolConfig, build_provider, builtin_profiles,
};

use crate::config::LoadedConfig;
use crate::daemon::{AgentCommandWatcher, RuntimeConnection};
use crate::i18n::Language;
use crate::{
    ApiArg, Cli, ProviderArg, TerminalApprover, TerminalSink, daemon, model_accepts_images,
    parse_api, parse_provider, projects, provider_config_from_profile, resolve_api_key,
    resolve_base, resolve_dialect, resolve_provider,
};

/// The some.im alias whose safety policy is managed on the gateway. It is a
/// reasoning model: it emits a long private rationale before the verdict tag,
/// which is why the judge must never cap its output tightly (see
/// [`willdeep_core::judge`]).
const SOMEIM_SECURITY_GUARD_MODEL: &str = "someim-security-guard";

/// The judge's model when `[agent] judge_model` is unset.
///
/// On some.im that is the dedicated `someim-security-guard` alias, whose
/// safety policy lives on the gateway and can be tightened server-side
/// without shipping a client. This matches the macOS app, so one operator
/// gets the same verdicts from the CLI and from Xedit. Every other provider
/// reuses the session's own model — there is no second endpoint to reach for,
/// and a judge that needs credentials the session does not have is a judge
/// that silently degrades into an approval card.
fn default_judge_model(kind: ProviderKind, session_model: &str) -> String {
    match kind {
        ProviderKind::SomeIm => SOMEIM_SECURITY_GUARD_MODEL.to_owned(),
        _ => session_model.to_owned(),
    }
}

/// The context compressor's model when `[agent] compressor_model` is unset.
///
/// Compression re-sends the whole older history in one request — the largest
/// single fixed cost in the loop. On some.im it goes to the gateway-hosted
/// `someim-32b-compressor` (flash-tier pricing; the fixed compression prompt
/// is injected server-side in replace mode, see muchtoken
/// docs/someim-32b-compressor.md). Every other provider keeps the session
/// model with the inline instruction.
const SOMEIM_CONTEXT_COMPRESSOR_MODEL: &str = "someim-32b-compressor";

/// Append one approval decision to `~/.willdeep/approvals.jsonl`. This is the
/// audit trail for "why did that command run without asking me" — and the
/// raw material for tuning the static rules. Best-effort: a logging failure
/// must never block a tool call.
fn record_approval_trace(path: &Path, trace: &ApprovalTrace) {
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    let source = match trace.source {
        ApprovalSource::StaticAllowlist => "static",
        ApprovalSource::Judge => "judge",
        ApprovalSource::AlwaysAllowList => "always-allow",
        ApprovalSource::User => "user",
    };
    let entry = serde_json::json!({
        "at": SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_secs())
            .unwrap_or_default(),
        "source": source,
        "detail": trace.detail,
        // The command is stored redacted: this file outlives the session.
        "command": willdeep_core::judge::redact_credentials(&trace.command),
    });
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    if let Ok(mut file) = options.open(path) {
        let _ = writeln!(file, "{entry}");
    }
}

pub(crate) enum HarnessFrontend {
    Terminal {
        json: bool,
        quiet: bool,
    },
    Tui {
        tx: crate::tui::TuiSender,
        relay: crate::mobile::RelayBridge,
    },
    Runtime {
        connection: RuntimeConnection,
        sink: Arc<dyn EventSink>,
        workspace_access: Option<crate::daemon::WorkspaceAccess>,
        allowed_skills: Vec<String>,
        allowed_mcp_servers: Vec<String>,
    },
}

pub(crate) struct BuiltHarness {
    pub agent: Arc<Agent>,
    pub workspace: PathBuf,
    pub skills: Arc<willdeep_core::SkillCatalog>,
    pub background_tasks: Arc<BackgroundTaskRegistry>,
    pub context_window: u64,
    pub provider_config: ProviderConfig,
    /// Built here so every frontend — TUI, headless run, runtime — shares one
    /// dispatcher and one set of `[notifications]` switches.
    pub notifier: crate::notify::Notifier,
    _command_watcher: Option<AgentCommandWatcher>,
}

pub(crate) struct HarnessOutcome {
    pub final_text: String,
    pub turns: usize,
    pub compressed: bool,
}

pub(crate) struct RuntimeHarnessOutcome {
    pub final_text: String,
    pub turns: usize,
    pub session_id: uuid::Uuid,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ExecutionOptions {
    pub allow_compress_command: bool,
    pub replay_existing_user_message: bool,
}

pub(crate) async fn execute_runtime(
    home: &Path,
    request: crate::daemon::SubmitTask,
    connection: RuntimeConnection,
    sink: Arc<dyn EventSink>,
) -> Result<RuntimeHarnessOutcome> {
    let loaded = LoadedConfig::load(request.config.as_deref())?;
    let language = Language::parse(loaded.file.agent.language.as_deref())?;
    let cli = Cli {
        command: None,
        prompt: Vec::new(),
        config: request.config.clone(),
        profile: request.profile.clone(),
        api_base: None,
        api_key: None,
        model: request.model.clone(),
        provider: None,
        api: None,
        workspace: Some(request.workspace.clone()),
        web_workspaces: Vec::new(),
        full_auto: false,
        max_turns: None,
        max_output_tokens: None,
        json: true,
        resume: request.session_id.map(|id| id.to_string()),
        list_sessions: false,
        list_projects: false,
        project: None,
        list_approvals: false,
        clear_approvals: false,
        no_tui: true,
        onboarding: false,
        web: false,
        listen: "127.0.0.1:9847".parse().expect("static listen address"),
        language: None,
        web_input_json: false,
    };
    let store = willdeep_core::SessionStore::new(home);
    let resumed = request.session_id.map(|id| store.load(id)).transpose()?;
    let built = build(
        &cli,
        &loaded,
        home,
        language,
        resumed.as_ref(),
        HarnessFrontend::Runtime {
            connection,
            sink,
            workspace_access: request.workspace_access,
            allowed_skills: request.workspace_skills.unwrap_or_default(),
            allowed_mcp_servers: request.workspace_mcp_servers.unwrap_or_default(),
        },
    )
    .await?;
    let mut session = resumed.unwrap_or_else(|| {
        willdeep_core::Session::new(
            built.workspace.clone(),
            request.profile.clone(),
            &request.prompt,
        )
    });
    if session.config.is_none() {
        session.config = request.config.clone();
    }
    let outcome = execute_noninteractive(
        &built,
        &store,
        &mut session,
        request.prompt,
        request.attachments,
        language,
        ExecutionOptions {
            allow_compress_command: true,
            replay_existing_user_message: request.replay_existing_user_message,
        },
    )
    .await?;
    Ok(RuntimeHarnessOutcome {
        final_text: outcome.final_text,
        turns: outcome.turns,
        session_id: session.id,
    })
}

pub(crate) async fn execute_noninteractive(
    built: &BuiltHarness,
    store: &willdeep_core::SessionStore,
    session: &mut willdeep_core::Session,
    prompt: String,
    attachments: Vec<willdeep_core::MessageAttachment>,
    language: Language,
    options: ExecutionOptions,
) -> Result<HarnessOutcome> {
    if options.allow_compress_command && prompt.trim() == "/compress" {
        let messages = built
            .agent
            .compress_history(session.messages.clone())
            .await?;
        let changed = session.replace_with_compressed_messages(messages);
        store.save(session)?;
        return Ok(HarnessOutcome {
            final_text: language
                .text(
                    if changed {
                        "上下文已压缩"
                    } else {
                        "当前上下文较短，无需压缩"
                    },
                    if changed {
                        "Context compressed"
                    } else {
                        "Context is too short to compress"
                    },
                    if changed {
                        "コンテキストを圧縮しました"
                    } else {
                        "コンテキストが短いため圧縮は不要です"
                    },
                )
                .to_owned(),
            turns: 0,
            compressed: true,
        });
    }
    let (history, user_message) = if options.replay_existing_user_message {
        let user_message = session
            .messages
            .last()
            .cloned()
            .context("recovered Turn is missing its persisted user message")?;
        (
            session.messages[..session.messages.len() - 1].to_vec(),
            user_message,
        )
    } else {
        let history = session.messages.clone();
        let user_message = willdeep_core::Message::user_with_attachments(&prompt, attachments);
        session.messages.push(user_message.clone());
        store.save(session)?;
        (history, user_message)
    };
    let mut outcome = built
        .agent
        .run_with_history_message(history, user_message)
        .await?;
    session.messages = outcome.messages.clone();
    store.save(session)?;
    loop {
        let events = built.background_tasks.drain_pending();
        if events.is_empty() {
            let running = built
                .background_tasks
                .snapshots()
                .iter()
                .any(|task| task.status == willdeep_core::BackgroundTaskStatus::Running);
            if !running {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            continue;
        }
        for event in events {
            outcome = built
                .agent
                .run_with_history(session.messages.clone(), event.notice)
                .await?;
            session.messages = outcome.messages.clone();
            store.save(session)?;
        }
    }
    // A headless run is exactly the case where nobody is watching the
    // terminal, so this is the ping that matters most — and the one that has to
    // be awaited: this function returns straight into process teardown, and a
    // detached delivery is dropped along with the runtime.
    built
        .notifier
        .set_session(&session.id.to_string(), Some(session.title.as_str()));
    built.notifier.task_completed(outcome.final_text.as_str());
    built.notifier.flush().await;
    if let Some(error) = built.notifier.take_error() {
        // There is no TUI notice line on this path, so stderr is the only way
        // the failure is not swallowed. stdout stays clean for --output json.
        eprintln!(
            "{}: {error}",
            language.text(
                "通知 Webhook 投递失败",
                "Notification webhook delivery failed",
                "通知 Webhook の送信に失敗しました"
            )
        );
    }
    Ok(HarnessOutcome {
        final_text: outcome.final_text,
        turns: outcome.turns,
        compressed: false,
    })
}

pub(crate) async fn build(
    cli: &Cli,
    loaded: &LoadedConfig,
    home: &Path,
    language: Language,
    resumed: Option<&willdeep_core::Session>,
    frontend: HarnessFrontend,
) -> Result<BuiltHarness> {
    let selected_profile_name = cli.profile.as_deref().or_else(|| {
        resumed
            .as_ref()
            .and_then(|session| session.profile.as_deref())
    });
    let profile = loaded.select_provider(selected_profile_name)?;
    let profile_provider = profile
        .and_then(|provider| provider.provider.as_deref())
        .map(parse_provider)
        .transpose()?;
    let selected_provider = cli.provider.or(profile_provider);
    let base = resolve_base(cli, profile, selected_provider)?;
    let kind = resolve_provider(selected_provider.unwrap_or(ProviderArg::Auto), &base);
    let profile_api = profile
        .and_then(|provider| provider.api.as_deref())
        .map(parse_api)
        .transpose()?;
    let dialect = resolve_dialect(cli.api.or(profile_api).unwrap_or(ApiArg::Auto), kind);
    let max_turns = cli.max_turns.or(loaded.file.agent.max_turns).unwrap_or(24);
    if !(1..=100).contains(&max_turns) {
        bail!("--max-turns must be between 1 and 100");
    }
    let workspace = resolve_workspace(cli, resumed)?;
    let api_key = resolve_api_key(cli, profile, kind)?;
    let model = cli
        .model
        .clone()
        .or_else(|| profile.and_then(|provider| provider.model.clone()))
        .or_else(|| (kind == ProviderKind::SomeIm).then(|| "glm-5".to_owned()))
        .context("model is required; set it in the provider profile, WILLDEEP_MODEL, or --model")?;
    let web_tools = (kind == ProviderKind::SomeIm).then(|| WebToolConfig {
        some_im_base_url: base.clone(),
        api_key: api_key.clone(),
    });
    let mut provider_config = ProviderConfig::new(kind, dialect, base, api_key, model.clone());
    provider_config.max_output_tokens = cli
        .max_output_tokens
        .or_else(|| profile.and_then(|provider| provider.max_output_tokens))
        .unwrap_or(16_384);
    let image_fallback = if kind == ProviderKind::SomeIm && !model_accepts_images(&model) {
        let vision_model = profile
            .and_then(|value| value.vision_model.clone())
            .unwrap_or_else(|| "qwen3-vl-plus".to_owned());
        let mut vision_config = provider_config.clone();
        vision_config.dialect = ApiDialect::ChatCompletions;
        vision_config.model = vision_model.clone();
        Some((
            build_provider(vision_config).context("initialize some.im vision fallback")?,
            vision_model,
        ))
    } else {
        None
    };
    let parent_provider_config = provider_config.clone();
    let provider = build_provider(provider_config).context("initialize provider")?;
    let configured_approval = loaded.file.agent.approval.as_deref().unwrap_or("smart");
    let runtime_access = match &frontend {
        HarnessFrontend::Runtime {
            workspace_access, ..
        } => *workspace_access,
        _ => None,
    };
    let approval_mode = if let Some(access) = runtime_access {
        match access {
            crate::daemon::WorkspaceAccess::ReadOnly => ApprovalMode::ReadOnly,
            crate::daemon::WorkspaceAccess::Smart => ApprovalMode::Smart,
            crate::daemon::WorkspaceAccess::WorkspaceWrite => ApprovalMode::WorkspaceAccess,
        }
    } else if cli.full_auto {
        ApprovalMode::WorkspaceAccess
    } else {
        match configured_approval {
            "strict" | "ask" | "request-every-time" => ApprovalMode::Strict,
            "smart" | "auto-review" => ApprovalMode::Smart,
            "workspace-write" | "workspace-access" => ApprovalMode::WorkspaceAccess,
            _ => bail!("agent.approval must be `strict`, `smart`, or `workspace-write`"),
        }
    };
    let (allowed_skills, allowed_mcp_servers) = match &frontend {
        HarnessFrontend::Runtime {
            allowed_skills,
            allowed_mcp_servers,
            ..
        } => (allowed_skills.as_slice(), allowed_mcp_servers.as_slice()),
        _ => (&[][..], &[][..]),
    };
    let skills = Arc::new(
        willdeep_core::SkillCatalog::discover(&workspace, &loaded.file.skills.roots)
            .allow_only(allowed_skills),
    );
    let mut mcp_servers = loaded.file.mcp_servers.clone();
    if !allowed_mcp_servers.is_empty() {
        mcp_servers.retain(|name, _| allowed_mcp_servers.contains(name));
    }
    let mcp = Arc::new(
        willdeep_core::McpRegistry::connect(&mcp_servers)
            .await
            .context("initialize MCP servers")?,
    );
    let (approver, sink, runtime_connection): (
        Arc<dyn Approver>,
        Arc<dyn EventSink>,
        Option<RuntimeConnection>,
    ) = match frontend {
        HarnessFrontend::Terminal { json, quiet } => (
            Arc::new(TerminalApprover(language)),
            Arc::new(TerminalSink { json, quiet }),
            None,
        ),
        HarnessFrontend::Tui { tx, relay } => (
            Arc::new(crate::tui::TuiApprover(tx.clone())),
            Arc::new(crate::tui::TuiSink { ui: tx, relay }),
            None,
        ),
        HarnessFrontend::Runtime {
            connection, sink, ..
        } => (
            daemon::runtime_approver(Some(&connection))?.context("initialize Runtime approver")?,
            sink,
            Some(connection),
        ),
    };
    let background_lifecycle_sink = sink.clone();
    let background_tasks = Arc::new(BackgroundTaskRegistry::default().with_lifecycle_observer(
        move |snapshot| {
            let sink = background_lifecycle_sink.clone();
            async move {
                if snapshot.kind != BackgroundTaskKind::Shell {
                    return;
                }
                if snapshot.status == BackgroundTaskStatus::Running {
                    sink.emit(willdeep_core::AgentEvent::BackgroundShellStarted {
                        id: snapshot.id,
                    })
                    .await;
                } else {
                    sink.emit(willdeep_core::AgentEvent::BackgroundShellCompleted {
                        id: snapshot.id,
                        status: snapshot.status,
                        exit_code: snapshot.exit_code,
                        elapsed_millis: snapshot.elapsed_millis,
                        output_bytes: snapshot.output_bytes,
                    })
                    .await;
                }
            }
        },
    ));
    let verification_home = home.to_path_buf();
    let verification_workspace = workspace.clone();
    // The judge answers one YES/NO per ambiguous command: on some.im that is
    // the gateway's managed `someim-security-guard` policy, elsewhere the
    // session's own model. `[agent] judge_model` overrides both.
    let safety_judge = if loaded.file.agent.safety_judge.unwrap_or(true) {
        let judge_model = loaded
            .file
            .agent
            .judge_model
            .clone()
            .unwrap_or_else(|| default_judge_model(kind, &model));
        let mut judge_config = parent_provider_config.clone();
        judge_config.model = judge_model.clone();
        Some(Arc::new(ProviderSafetyJudge::new(
            build_provider(judge_config).context("initialize safety judge provider")?,
            judge_model,
        )) as Arc<dyn SafetyJudge>)
    } else {
        None
    };
    let approval_log = home.join("approvals.jsonl");
    let mut tools = ToolRegistry::new(&workspace, approval_mode)?
        .with_approver(approver)
        .with_approval_reporter(move |trace| {
            record_approval_trace(&approval_log, &trace);
        })
        .with_skills(skills.clone())
        .with_mcp(mcp)
        .with_background_tasks(background_tasks.clone())
        .with_verification_reporter(move |verification| {
            let home = verification_home.clone();
            let workspace = verification_workspace.clone();
            let Ok(snapshot) = daemon::diff_review::snapshot(&workspace) else {
                return;
            };
            tokio::spawn(async move {
                let _ = daemon::diff_review::remote_record_verification(
                    &home,
                    &workspace,
                    snapshot.id,
                    verification,
                )
                .await;
            });
        })
        .with_web_tools(web_tools)
        // Only the main agent is told a failure is delegable: a subagent
        // cannot spawn anything, so the hint would be an instruction it has
        // no way to act on.
        .with_delegation_hints(true)
        .with_always_allow_store(home.join("always-allow.json"))?;
    if let Some(judge) = safety_judge.clone() {
        tools = tools.with_safety_judge(judge);
    }
    let tools = tools;
    let mut system_prompt = willdeep_core::prompt::build_system_prompt(&workspace);
    if !skills.list().is_empty() {
        system_prompt.push_str(
            "\n\n# Available skills\nUse list_skills to search and read_skill before applying a relevant skill. Entries may carry a tier: `tier=worker` marks a skill whose steps fit a small-context worker — prefer dispatching it via spawn_agent with a task packet instead of running it inline; `tier=deep` marks work that needs the largest window available. Untagged skills run at the session's default tier.\n",
        );
        system_prompt.push_str(&skills.routing_summary(4_096));
    }
    let context_window = profile
        .and_then(|value| value.context_window)
        .unwrap_or(128_000);
    let cheap_model = if kind == ProviderKind::SomeIm {
        "glm-5".to_owned()
    } else {
        model.clone()
    };
    let cheap_provider = if kind == ProviderKind::SomeIm {
        let mut cheap = parent_provider_config.clone();
        cheap.model = cheap_model.clone();
        build_provider(cheap).context("initialize default subagent provider")?
    } else {
        provider.clone()
    };
    let mut subagent_profiles = builtin_profiles(provider.clone(), cheap_provider, context_window);
    for subagent in &mut subagent_profiles {
        // On some.im each trade runs its own relay-hosted virtual model
        // (`someim-32b-<trade>`), exactly as the macOS app binds it: same
        // gateway, same account, so the same trade must resolve to the same
        // model in both clients. The relay prepends that trade's job prompt,
        // so the client stops sending its own copy. Trades the relay does not
        // host fall back to the base cheap model with the client prompt.
        let hosted = (kind == ProviderKind::SomeIm)
            .then(|| willdeep_core::subagent::hosted_worker_model(&subagent.id))
            .flatten();
        subagent.model = Some(if subagent.id == "deep" {
            model.clone()
        } else {
            hosted.clone().unwrap_or_else(|| cheap_model.clone())
        });
        if let Some(hosted_model) = &hosted {
            let mut configured = parent_provider_config.clone();
            configured.model = hosted_model.clone();
            subagent.provider = build_provider(configured)
                .with_context(|| format!("initialize hosted subagent model {hosted_model}"))?;
            subagent.hosted_job_prompt = true;
        }
        if let Some(settings) = loaded.file.subagents.get(&subagent.id) {
            if let Some(provider_name) = settings.provider_profile.as_deref() {
                let mut configured = provider_config_from_profile(&loaded.file, provider_name)?;
                if let Some(model) = &settings.model {
                    configured.model = model.clone();
                }
                subagent.model = Some(configured.model.clone());
                subagent.hosted_job_prompt = hosted.as_deref() == Some(configured.model.as_str());
                subagent.provider = build_provider(configured)
                    .with_context(|| format!("initialize subagent profile {}", subagent.id))?;
            } else if let Some(model) = &settings.model {
                let mut configured = parent_provider_config.clone();
                configured.model = model.clone();
                subagent.model = Some(model.clone());
                // An explicitly bound model carries no hosted job prompt, so
                // the client sends its own again — a trade must never end up
                // with no job description at all.
                subagent.hosted_job_prompt = hosted.as_deref() == Some(model.as_str());
                subagent.provider = build_provider(configured)
                    .with_context(|| format!("initialize subagent profile {}", subagent.id))?;
            }
            if let Some(max_turns) = settings.max_turns {
                subagent.max_turns = max_turns;
            }
            if let Some(window) = settings.context_window {
                subagent.context_window = window;
            }
            if let Some(token_budget) = settings.token_budget {
                subagent.token_budget = Some(token_budget);
            }
            if let Some(timeout_seconds) = settings.timeout_seconds {
                subagent.timeout_seconds = Some(timeout_seconds);
            }
            if let Some(max_failures) = settings.max_consecutive_failures {
                subagent.max_consecutive_failures = max_failures;
            }
            if let Some(limit) = settings.tool_output_limit {
                subagent.tool_output_limit = Some(limit);
            }
            if let Some(max_attempts) = settings.max_attempts {
                subagent.max_attempts = max_attempts;
            }
            if let Some(worktree) = settings.worktree.as_deref() {
                subagent.worktree = match worktree {
                    "dedicated" => willdeep_core::SubagentWorktreePolicy::Dedicated,
                    _ => willdeep_core::SubagentWorktreePolicy::Shared,
                };
            }
        }
    }
    let mut catalog = SubagentCatalog::new(&workspace, subagent_profiles, background_tasks.clone())
        .with_worktree_root(home.join("worktrees").join("subagents"))
        // Task packets may name a skill; the runtime inlines its body so the
        // worker never spends turns fetching its own instructions.
        .with_skills(skills.clone())
        .with_event_sink(sink.clone());
    // Verifier commands run unattended, with no approval card to fall back
    // on. They go through the same judge the main agent's shell does.
    if let Some(judge) = safety_judge {
        catalog = catalog.with_safety_judge(judge);
    }
    let subagents = Arc::new(catalog);
    let command_watcher = daemon::start_agent_command_watcher(
        runtime_connection.as_ref(),
        background_tasks.clone(),
        subagents.clone(),
    )?;
    let goal_continuation = Arc::new(willdeep_core::GoalContinuation::new());
    if let Some(goal) = resumed.and_then(|session| session.goal.as_deref()) {
        goal_continuation.activate(goal, willdeep_core::GoalBudget::default());
    }
    let mut agent = Agent::new(
        provider,
        tools,
        AgentConfig {
            max_turns,
            system_prompt,
            context_window,
            token_budget: None,
        },
    )
    .with_event_sink(sink)
    .with_subagents(subagents)
    .with_goal_continuation(goal_continuation.clone())
    .with_background_tasks(background_tasks.clone());
    if loaded.file.agent.small_model_routing.unwrap_or(true) {
        agent = agent.with_routing_guard(Arc::new(RoutingGuard::new(RoutingPolicy {
            auto_dispatch_read_only: loaded.file.agent.auto_dispatch_read_only.unwrap_or(true),
            max_deep_calls: loaded.file.agent.max_deep_calls_per_harness.unwrap_or(1),
        })));
    }
    if let Some((vision_provider, vision_model)) = image_fallback {
        agent = agent.with_image_fallback(vision_provider, format!("some.im / {vision_model}"));
    }
    // 压缩摘要模型：some.im 默认绑网关托管的 someim-32b-compressor，
    // `[agent] compressor_model` 可覆盖。只有恰好绑到托管模型时才省掉
    // 行内指令（hosted_prompt）——覆盖成别的模型时指令必须跟着请求走，
    // 否则压缩调用没有任务描述。其它 provider 未配置时维持会话模型。
    let compressor_model = loaded.file.agent.compressor_model.clone().or_else(|| {
        (kind == ProviderKind::SomeIm).then(|| SOMEIM_CONTEXT_COMPRESSOR_MODEL.to_owned())
    });
    if let Some(compressor_model) = compressor_model {
        let hosted_prompt =
            kind == ProviderKind::SomeIm && compressor_model == SOMEIM_CONTEXT_COMPRESSOR_MODEL;
        let mut compressor_config = parent_provider_config.clone();
        compressor_config.model = compressor_model;
        agent = agent.with_compressor(
            build_provider(compressor_config).context("initialize context compressor provider")?,
            hosted_prompt,
        );
    }
    let notifier = crate::notify::Notifier::new(&loaded.file.notifications);
    Ok(BuiltHarness {
        agent: Arc::new(agent),
        workspace,
        skills,
        background_tasks,
        context_window,
        provider_config: parent_provider_config,
        notifier,
        _command_watcher: command_watcher,
    })
}

pub(crate) fn resolve_workspace(
    cli: &Cli,
    resumed: Option<&willdeep_core::Session>,
) -> Result<PathBuf> {
    let project_workspace = cli.project.as_deref().map(projects::resolve).transpose()?;
    let requested_workspace = cli
        .workspace
        .clone()
        .or(project_workspace)
        .or_else(|| resumed.map(|session| session.workspace.clone()))
        .unwrap_or_else(|| PathBuf::from("."));
    requested_workspace
        .canonicalize()
        .with_context(|| format!("invalid workspace: {}", requested_workspace.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn someim_sessions_judge_with_the_managed_security_guard() {
        assert_eq!(
            default_judge_model(ProviderKind::SomeIm, "someim-auto-flash"),
            "someim-security-guard",
            "a some.im session must reach the gateway's managed safety policy, \
             matching the macOS app"
        );
    }

    #[test]
    fn every_other_provider_reuses_the_session_model() {
        // No second endpoint, no second credential: a judge the session cannot
        // authenticate is a judge that degrades into an approval card.
        assert_eq!(
            default_judge_model(ProviderKind::OpenAiCompatible, "deepseek-v4-flash"),
            "deepseek-v4-flash"
        );
        assert_eq!(
            default_judge_model(ProviderKind::Anthropic, "claude-opus-5"),
            "claude-opus-5"
        );
    }
}
