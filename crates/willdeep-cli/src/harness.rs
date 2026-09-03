use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use willdeep_core::hooks::HookRegistry;
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

/// 把三个 Worker 档位各自解析成一个模型绑定。
///
/// 优先级从高到低：
///
/// 1. 用户显式写的 `[worker_tiers.<档>]`——这是调用点的意志，压过一切默认；
/// 2. some.im 网关上这一档的默认模型（[`willdeep_core::WorkerTier::default_hosted_model`]），
///    与 Xedit 同一张表：同一个人换个客户端不该换个 Worker；
/// 3. 专家档在非网关 provider 上回落父模型——别处没有那张表里的模型，回落到
///    会话自己的模型至少还是「更强的那个」；
/// 4. 其余不绑定：只放宽预算，不偷偷换模型。
///
/// 基础档默认不绑定。它已经是工种自己的模型了，再绑一次只会把用户在
/// `[subagents.*]` 里的选择覆盖掉。
fn resolve_tier_bindings(
    file: &crate::config::ConfigFile,
    parent: &ProviderConfig,
    kind: ProviderKind,
    session_window: u64,
) -> Result<Vec<(willdeep_core::WorkerTier, willdeep_core::TierBinding)>> {
    let mut bindings = Vec::new();
    for tier in willdeep_core::WorkerTier::ALL {
        let configured = file.worker_tiers.get(tier.as_str());
        let mut provider_config =
            match configured.and_then(|value| value.provider_profile.as_deref()) {
                // 这里是 fail-fast：绑了却建不起来（多半是那个 Profile 的密钥
                // 没配）就不让会话启动。静默跳过更糟——票据照扣，档没升，而
                // 用户要等到某次派工完成之后才可能发现。
                Some(name) => provider_config_from_profile(file, name).with_context(|| {
                    format!("resolve worker_tiers.{}.provider_profile", tier.as_str())
                })?,
                None => parent.clone(),
            };
        let explicit_model = configured.and_then(|value| value.model.as_deref());
        let default_model = (kind == ProviderKind::SomeIm
            && tier != willdeep_core::WorkerTier::Standard)
            .then(|| tier.default_hosted_model());
        let Some(model) = explicit_model
            .or(default_model)
            .map(str::to_owned)
            .or_else(|| {
                // 专家档在别的 provider 上没有网关那张表可查，回落父模型——这是
                // 正交化之前的行为，保住它至少不会让票据白扣。
                (tier == willdeep_core::WorkerTier::Expert).then(|| parent.model.clone())
            })
        else {
            continue;
        };
        provider_config.model = model.clone();
        let window = configured
            .and_then(|value| value.context_window)
            .unwrap_or_else(|| match tier {
                // 专家档兑现的是整个会话预算——这一档贵到要票据，就是因为它
                // 把会话窗口整个交给一个 Worker。
                willdeep_core::WorkerTier::Expert => tier.context_budget().max(session_window),
                _ => tier.context_budget(),
            });
        let binding = willdeep_core::TierBinding {
            provider: build_provider(provider_config).with_context(|| {
                format!("initialize {} worker tier model {model}", tier.as_str())
            })?,
            hosted_job_prompt: willdeep_core::hosts_job_prompt(&model),
            model: Some(model),
            window,
        };
        bindings.push((tier, binding));
    }
    Ok(bindings)
}

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

/// 因为有待处理的运行时事件而开的那一轮，用它当提示词。
///
/// 短，且不带任何事实：真正的内容由内核在 turn 顶部注入，正文经过净化并标明
/// 了来源。把事件正文塞进提示词等于让它绕过那一层。
pub(crate) const KERNEL_WAKE_PROMPT: &str =
    "Runtime events arrived while you were away. Review them and continue.";

pub(crate) struct BuiltHarness {
    pub agent: Arc<Agent>,
    pub workspace: PathBuf,
    pub skills: Arc<willdeep_core::SkillCatalog>,
    pub background_tasks: Arc<BackgroundTaskRegistry>,
    /// 宿主事件内核。前端把后台任务、入站通知交给它，主 Agent 在 turn 边界
    /// 收走——两条路只能留一条，否则同一个结果会向模型讲两遍。
    pub kernel: willdeep_core::EventKernel,
    /// 事件日志。前端在状态变化后刷盘，进程重启时由这里恢复。
    pub kernel_store: willdeep_core::kernel_store::KernelStore,
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
    // L1：提交那一刻就把占位标题换掉。历史列表要在轮次跑完之前就可读——
    // 一条正在跑的会话恰恰是人最可能去列表里找的那条。
    if crate::titling::apply_derived_title(session, &prompt, !attachments.is_empty()) {
        store.save(session)?;
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
    // 一次 AI 回合的遥测：供应商类型、模型、耗时、token、结构化错误码。
    // 提示词与回复正文一个字都不上报（协议里也没有能装下它们的字段）。
    let turn_telemetry = crate::telemetry::TurnTelemetry::start(
        built.provider_config.kind,
        &built.provider_config.base_url,
        &built.provider_config.model,
        !built.provider_config.api_key.is_empty(),
    );
    let run_result = built
        .agent
        .run_with_history_message(history, user_message)
        .await;
    match &run_result {
        Ok(outcome) => turn_telemetry.finish(
            crate::telemetry::global(),
            Ok((outcome.input_tokens, outcome.output_tokens)),
        ),
        Err(error) => turn_telemetry.finish(crate::telemetry::global(), Err(error)),
    }
    let mut outcome = run_result?;
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
        // 后台结果走事件内核，不再一条通知起一轮。
        //
        // 两个好处：同时结束的几个任务合成一轮交给模型，而不是逼它把同一段
        // 上下文重读三遍；正文按来源净化过，Worker 的自述不会冒充系统消息。
        // 这里**不再**把 notice 当提示词——那样等于同一个结果讲两遍。
        for event in events {
            built.kernel.publish(
                willdeep_core::kernel::background_task_event(
                    session.id,
                    &event.snapshot,
                    event.notice,
                ),
                willdeep_core::DedupPolicy::Once,
            );
        }
        outcome = built
            .agent
            .run_with_history(session.messages.clone(), KERNEL_WAKE_PROMPT.to_owned())
            .await?;
        session.messages = outcome.messages.clone();
        store.save(session)?;
        willdeep_core::kernel_store::flush(&built.kernel, &built.kernel_store);
    }
    // L2：第一轮问答落地后跑一次摘要。放在通知之前，好让 webhook 带上的是
    // 整理过的标题而不是提示词前缀。
    if crate::titling::apply_summarized_title(&built.agent, session).await {
        store.save(session)?;
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
    let local_auxiliary_config = local_auxiliary_provider_config(&loaded.file.local_model);
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
    let sandbox = resolve_sandbox(&loaded.file.agent, approval_mode, &workspace);
    let hooks = build_hooks(&loaded.file.hooks).context("read [[hooks]]")?;
    let mut tools = ToolRegistry::new(&workspace, approval_mode)?
        .with_sandbox(sandbox)
        .with_hooks(hooks)
        .with_approver(approver)
        .with_approval_reporter(move |trace| {
            record_approval_trace(&approval_log, &trace);
        })
        .with_skills(skills.clone())
        .with_mcp(mcp.clone())
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
    let mut subagent_profiles = builtin_profiles(cheap_provider);
    for subagent in &mut subagent_profiles {
        // some.im 上基础档统一是 `someim-32b`：同一个网关、同一批账号，同一个
        // 职责在两个客户端必须解析到同一个模型。历史上的 `someim-32b-<工种>`
        // 已经退役，职责提示词改由客户端随请求发送。
        let hosted = (kind == ProviderKind::SomeIm)
            .then(|| willdeep_core::subagent::hosted_worker_model(&subagent.id))
            .flatten();
        // 每个工种都走自己的托管绑定，没有绑定时回落便宜模型。没有哪个职责
        // 天生配父模型——那是 WorkerTier::Expert 的事，而那一档要票据。
        subagent.model = Some(hosted.clone().unwrap_or_else(|| cheap_model.clone()));
        if let Some(hosted_model) = &hosted {
            let mut configured = parent_provider_config.clone();
            configured.model = hosted_model.clone();
            subagent.provider = build_provider(configured)
                .with_context(|| format!("initialize hosted subagent model {hosted_model}"))?;
        }
        if let Some(settings) = loaded.file.subagents.get(&subagent.id) {
            if let Some(provider_name) = settings.provider_profile.as_deref() {
                let mut configured = provider_config_from_profile(&loaded.file, provider_name)?;
                if let Some(model) = &settings.model {
                    configured.model = model.clone();
                }
                subagent.model = Some(configured.model.clone());
                subagent.provider = build_provider(configured)
                    .with_context(|| format!("initialize subagent profile {}", subagent.id))?;
            } else if let Some(model) = &settings.model {
                let mut configured = parent_provider_config.clone();
                configured.model = model.clone();
                subagent.model = Some(model.clone());
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
        // 判定放在所有覆盖之后，跟着**最终**解析出的模型走。跟着工种名走是
        // 错的：工种绑成 `someim-32b` 时网关并不会 prepend 职责提示词，客户端
        // 若也把自己那份省掉，Worker 就只剩边界段落、不知道自己是干什么的。
        subagent.hosted_job_prompt = subagent
            .model
            .as_deref()
            .is_some_and(willdeep_core::hosts_job_prompt);
    }
    let mut catalog = SubagentCatalog::new(&workspace, subagent_profiles, background_tasks.clone())
        .with_worktree_root(home.join("worktrees").join("subagents"))
        // Task packets may name a skill; the runtime inlines its body so the
        // worker never spends turns fetching its own instructions.
        .with_skills(skills.clone())
        // 兜底工种能查已连接的 MCP 服务。窄工种拿不到：它们的价值就是范围窄。
        .with_mcp(mcp)
        // Worker 与父会话共享已批准的精确动作。没有这条，后台 Worker 会在人
        // 刚刚批过的同一条命令上再卡一次，而它自己没有审批 UI。
        .with_always_allow_store(home.join("always-allow.json"))
        .with_event_sink(sink.clone());
    // 档位兑现成哪个模型。准入在 agent 层，这里只负责兑现。
    for (tier, binding) in
        resolve_tier_bindings(&loaded.file, &parent_provider_config, kind, context_window)?
    {
        catalog = catalog.with_tier_binding(tier, binding);
    }
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
    // 事件内核先于 Agent 建起来，并立刻把上一次运行没投递完的事件读回来。
    // 读不出来不挡启动：事件是通知不是账本，为一份坏日志让整个 Runtime 起不
    // 来才是真的坏。
    let kernel = willdeep_core::EventKernel::new();
    let kernel_store = willdeep_core::kernel_store::KernelStore::new(home);
    let restored = willdeep_core::kernel_store::restore_into(&kernel, &kernel_store);
    for (path, reason) in &restored.quarantined {
        eprintln!(
            "willdeep: quarantined a damaged event log ({reason}); kept at {}",
            path.display()
        );
    }
    let goal_continuation = Arc::new(willdeep_core::GoalContinuation::new());
    if let Some(goal) = resumed.and_then(|session| session.goal.as_deref()) {
        goal_continuation.activate(goal, willdeep_core::GoalBudget::default());
    }
    let mut agent = Agent::new(
        provider.clone(),
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
    .with_background_tasks(background_tasks.clone())
    .with_event_kernel(kernel.clone());
    if loaded.file.agent.small_model_routing.unwrap_or(true) {
        let mut routing = RoutingGuard::new(RoutingPolicy {
            auto_dispatch_read_only: loaded.file.agent.auto_dispatch_read_only.unwrap_or(true),
            max_deep_calls: loaded.file.agent.max_deep_calls_per_harness.unwrap_or(1),
        });
        if loaded.file.local_model.enabled
            && loaded.file.local_model.prefer_for_worker_routing
            && let Some(local_config) = local_auxiliary_config.clone()
        {
            routing = routing.with_classifiers(vec![
                build_provider(local_config).context("initialize local routing model")?,
                provider.clone(),
            ]);
        }
        agent = agent.with_routing_guard(Arc::new(routing));
    }
    if let Some((vision_provider, vision_model)) = image_fallback {
        agent = agent.with_image_fallback(vision_provider, format!("some.im / {vision_model}"));
    }
    // 与 Swift App 同一候选顺序：显式偏好的本地辅助模型 → some.im 托管压缩器
    // （仅在没有显式 compressor_model 时）/显式模型 → 会话模型兜底。
    let mut compressors = Vec::new();
    if loaded.file.local_model.enabled
        && loaded.file.local_model.prefer_for_context_summaries
        && let Some(local_config) = local_auxiliary_config.clone()
    {
        compressors.push((
            build_provider(local_config).context("initialize local context summary model")?,
            false,
        ));
    }
    if let Some(compressor_model) = loaded.file.agent.compressor_model.clone() {
        let mut compressor_config = parent_provider_config.clone();
        compressor_config.model = compressor_model;
        compressors.push((
            build_provider(compressor_config).context("initialize context compressor provider")?,
            false,
        ));
    } else if kind == ProviderKind::SomeIm {
        let mut compressor_config = parent_provider_config.clone();
        compressor_config.model = SOMEIM_CONTEXT_COMPRESSOR_MODEL.to_owned();
        compressors.push((
            build_provider(compressor_config).context("initialize hosted context compressor")?,
            true,
        ));
    }
    compressors.push((provider.clone(), false));
    agent = agent.with_compressors(compressors);

    // 标题同样本地优先、会话 Provider 兜底；请求仍只带一问一答各 800 字。
    if loaded.file.agent.auto_title.unwrap_or(true) {
        let mut titlers = Vec::new();
        if loaded.file.local_model.enabled
            && loaded.file.local_model.prefer_for_titles
            && let Some(local_config) = local_auxiliary_config
        {
            titlers.push(
                build_provider(local_config).context("initialize local session title model")?,
            );
        }
        let mut title_config = parent_provider_config.clone();
        if let Some(title_model) = loaded.file.agent.title_model.clone() {
            title_config.model = title_model;
        }
        titlers.push(build_provider(title_config).context("initialize session title provider")?);
        agent = agent.with_titlers(titlers);
    }
    let notifier = crate::notify::Notifier::new(&loaded.file.notifications);
    Ok(BuiltHarness {
        agent: Arc::new(agent),
        workspace,
        skills,
        background_tasks,
        kernel,
        kernel_store,
        context_window,
        provider_config: parent_provider_config,
        notifier,
        _command_watcher: command_watcher,
    })
}

fn local_auxiliary_provider_config(
    settings: &crate::config::LocalModelSettings,
) -> Option<ProviderConfig> {
    if !settings.enabled {
        return None;
    }
    let mut config = ProviderConfig::new(
        ProviderKind::OpenAiCompatible,
        ApiDialect::ChatCompletions,
        settings.base_url.trim(),
        "",
        settings.summary_model.trim(),
    );
    // “本地”描述的是用户自建的辅助算力，不限定部署在当前进程所在机器。
    // 家庭局域网、内网域名和回环地址都可以明确选择免 Token；普通 Provider
    // Profile 没有这个标记，缺 Key 时仍然拒绝启动。
    config.allow_unauthenticated = true;
    // Swift App caps Worker routing at eight seconds. Reusing the same local
    // client timeout keeps every auxiliary fallback bounded as well.
    config.request_timeout_secs = 8;
    Some(config)
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

/// 把配置里的 `[[hooks]]` 翻成注册表。
///
/// 配置错误在这里**直接失败**，不静默跳过：一条拼错事件名的门禁 hook 如果被
/// 悄悄忽略，用户会以为门禁在生效，而它一次都不会触发——这比没配还危险。
fn build_hooks(settings: &[crate::config::HookSettings]) -> Result<HookRegistry> {
    use willdeep_core::hooks::{Hook, HookEvent, HookFailure};

    let mut hooks = Vec::with_capacity(settings.len());
    for entry in settings {
        let event = match entry.event.as_str() {
            "pre_tool" => HookEvent::PreTool,
            "post_tool" => HookEvent::PostTool,
            "approval_resolved" => HookEvent::ApprovalResolved,
            other => bail!(
                "hooks.event 只能是 `pre_tool`、`post_tool` 或 `approval_resolved`，收到 `{other}`"
            ),
        };
        let on_error = match entry.on_error.as_deref() {
            None | Some("deny") => HookFailure::Deny,
            Some("ignore") => HookFailure::Ignore,
            Some(other) => bail!("hooks.on_error 只能是 `deny` 或 `ignore`，收到 `{other}`"),
        };
        if entry.command.trim().is_empty() {
            bail!("hooks.command 不能为空");
        }
        if entry.blocking && !event.can_block() {
            bail!(
                "`{}` 事件发生在动作之后，blocking = true 拦不住任何东西；\
                 想拦请改用 `pre_tool`",
                entry.event
            );
        }
        hooks.push(Hook {
            name: entry.name.clone().unwrap_or_else(|| entry.event.clone()),
            event,
            command: entry.command.clone(),
            blocking: entry.blocking,
            timeout: std::time::Duration::from_secs(entry.timeout_seconds.unwrap_or(10)),
            on_error,
        });
    }
    Ok(HookRegistry::new(hooks))
}

/// 把审批档位翻译成 OS 侧的围栏档位，并算出可写根。
///
/// 不新造一个轴：围栏档位是工作区策略的投影，用户已经选过一次的东西不该再选
/// 第二次。临时目录默认可写——不给的话 `cargo`、`rustc`、`git` 全都写不了中间
/// 文件，围栏第一天就会被关掉。
fn resolve_sandbox(
    agent: &crate::config::AgentSettings,
    approval_mode: ApprovalMode,
    workspace: &std::path::Path,
) -> willdeep_core::sandbox::SandboxSpec {
    use willdeep_core::sandbox::{SandboxPolicy, SandboxSpec};

    if !agent.sandbox.unwrap_or(false) {
        return SandboxSpec::new(SandboxPolicy::Off, []);
    }
    let policy = match approval_mode {
        ApprovalMode::ReadOnly => SandboxPolicy::ReadOnly,
        _ => SandboxPolicy::WorkspaceWrite,
    };
    let mut roots = vec![workspace.to_path_buf(), std::env::temp_dir()];
    roots.extend(agent.sandbox_writable_roots.iter().cloned());
    SandboxSpec::new(policy, roots)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parent_config(kind: ProviderKind, model: &str) -> ProviderConfig {
        ProviderConfig::new(
            kind,
            ApiDialect::ChatCompletions,
            "https://some.im/v1".to_owned(),
            "placeholder".to_owned(),
            model.to_owned(),
        )
    }

    fn bound_models(
        source: &str,
        kind: ProviderKind,
        parent_model: &str,
    ) -> Vec<(String, String, u64)> {
        let file: crate::config::ConfigFile =
            toml::from_str(source).expect("parse tier config fixture");
        resolve_tier_bindings(&file, &parent_config(kind, parent_model), kind, 400_000)
            .expect("resolve tier bindings")
            .into_iter()
            .map(|(tier, binding)| {
                (
                    tier.as_str().to_owned(),
                    binding.model.unwrap_or_default(),
                    binding.window,
                )
            })
            .collect()
    }

    #[test]
    fn the_gateway_tier_table_is_wired_not_just_documented() {
        // 0.51 的毛病：`default_hosted_model()` 那张表只有测试引用，运行时
        // 压根没查过它，于是进阶档不换模型、专家档默默用了会话自己的模型。
        let bound = bound_models("version = 1\n", ProviderKind::SomeIm, "glm-5");
        assert_eq!(
            bound,
            vec![
                (
                    "advanced".to_owned(),
                    "deepseek-v4-flash".to_owned(),
                    262_144
                ),
                // 专家档兑现整个会话预算，这正是它要票据的理由。
                ("expert".to_owned(), "gpt-5.6-sol".to_owned(), 400_000),
            ],
            "基础档不该被绑定——那会盖掉用户在 [subagents.*] 里的选择"
        );
    }

    #[test]
    fn other_providers_keep_the_parent_model_for_the_expert_tier() {
        // 别处没有网关那张表里的模型。回落父模型是正交化之前的行为，保住它
        // 至少不会让票据白扣；进阶档则宁可不换，也不偷偷换成一个没人指定的。
        let bound = bound_models("version = 1\n", ProviderKind::Anthropic, "opus-5");
        assert_eq!(
            bound,
            vec![("expert".to_owned(), "opus-5".to_owned(), 400_000)]
        );
    }

    #[test]
    fn explicit_worker_tier_config_outranks_the_gateway_defaults() {
        let bound = bound_models(
            r#"
version = 1

[worker_tiers.standard]
model = "qwen3-32b"
context_window = 131072

[worker_tiers.expert]
model = "opus-5"
context_window = 500000
"#,
            ProviderKind::SomeIm,
            "glm-5",
        );
        assert_eq!(
            bound,
            vec![
                ("standard".to_owned(), "qwen3-32b".to_owned(), 131_072),
                // 没配的档仍然回落网关默认表。
                (
                    "advanced".to_owned(),
                    "deepseek-v4-flash".to_owned(),
                    262_144
                ),
                ("expert".to_owned(), "opus-5".to_owned(), 500_000),
            ]
        );
    }

    #[test]
    fn a_tier_bound_to_a_legacy_alias_still_defers_to_the_relay_prompt() {
        // 用户仍然可以显式指回那七个退役别名。指回去了，网关就还是会 prepend
        // 职责提示词，客户端这时必须闭嘴，否则同一份职责说两遍。
        let file: crate::config::ConfigFile =
            toml::from_str("version = 1\n[worker_tiers.advanced]\nmodel = \"someim-32b-reader\"\n")
                .expect("parse legacy alias config");
        let bindings = resolve_tier_bindings(
            &file,
            &parent_config(ProviderKind::SomeIm, "glm-5"),
            ProviderKind::SomeIm,
            400_000,
        )
        .expect("resolve tier bindings");
        let advanced = bindings
            .iter()
            .find(|(tier, _)| *tier == willdeep_core::WorkerTier::Advanced)
            .expect("advanced tier");
        assert!(advanced.1.hosted_job_prompt);
        // 而网关默认的那两个模型都不是托管别名。
        let expert = bindings
            .iter()
            .find(|(tier, _)| *tier == willdeep_core::WorkerTier::Expert)
            .expect("expert tier");
        assert!(!expert.1.hosted_job_prompt);
    }

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
