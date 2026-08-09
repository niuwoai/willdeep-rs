use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use clap::{Parser, ValueEnum};
use willdeep_core::provider::{ApiDialect, ProviderConfig, ProviderKind};
use willdeep_core::{
    Agent, AgentConfig, AgentEvent, ApprovalDecision, ApprovalMode, Approver,
    BackgroundTaskRegistry, EventSink, SubagentCatalog, ToolRegistry, UserQuestion, WebToolConfig,
    build_provider, builtin_profiles,
};

mod config;
mod editor;
mod mobile;
mod onboarding;
mod projects;
mod tui;

use config::{LoadedConfig, ProviderProfile, willdeep_home};

#[derive(Parser, Debug)]
#[command(
    name = "willdeep",
    version,
    about = "Cross-platform WillDeep coding agent"
)]
struct Cli {
    /// Task for the agent. Reads stdin when omitted.
    #[arg(value_name = "PROMPT", num_args = 0.., trailing_var_arg = true)]
    prompt: Vec<String>,

    /// TOML configuration path. Defaults to $WILLDEEP_HOME/config.toml or ~/.willdeep/config.toml.
    #[arg(long, env = "WILLDEEP_CONFIG")]
    config: Option<PathBuf>,

    /// Provider profile name from the TOML configuration.
    #[arg(long)]
    profile: Option<String>,

    /// Provider API base, for example https://some.im/v1.
    #[arg(long, env = "WILLDEEP_API_BASE")]
    api_base: Option<String>,

    /// Provider API key. Prefer the environment variable to shell history.
    #[arg(long, env = "WILLDEEP_API_KEY", hide_env_values = true)]
    api_key: Option<String>,

    /// Model identifier sent to the provider.
    #[arg(long, env = "WILLDEEP_MODEL")]
    model: Option<String>,

    /// Provider identity controls authentication and some.im context headers.
    #[arg(long, value_enum)]
    provider: Option<ProviderArg>,

    /// Wire API dialect. Auto selects Anthropic Messages only for api.anthropic.com; otherwise Chat Completions.
    #[arg(long = "api", value_enum)]
    api: Option<ApiArg>,

    /// Workspace root available to tools.
    #[arg(long)]
    workspace: Option<PathBuf>,

    /// Allow create/edit inside the workspace without approval. Shell and MCP still ask.
    #[arg(long)]
    full_auto: bool,

    /// Maximum model/tool rounds.
    #[arg(long)]
    max_turns: Option<usize>,

    /// Maximum output tokens for Anthropic Messages.
    #[arg(long)]
    max_output_tokens: Option<u32>,

    /// Emit newline-delimited JSON events on stdout.
    #[arg(long)]
    json: bool,

    /// Resume a saved session by UUID, or use `latest`.
    #[arg(long, value_name = "ID|latest")]
    resume: Option<String>,

    /// List saved sessions and exit.
    #[arg(long)]
    list_sessions: bool,

    /// List projects shared with the Swift app and exit (macOS).
    #[arg(long)]
    list_projects: bool,

    /// Select a shared Swift project by name or UUID.
    #[arg(long)]
    project: Option<String>,

    /// List persisted exact-command and MCP Always Allow rules, then exit.
    #[arg(long)]
    list_approvals: bool,

    /// Clear all persisted Always Allow rules, then exit.
    #[arg(long)]
    clear_approvals: bool,

    /// Disable the interactive TUI when no prompt argument is supplied.
    #[arg(long)]
    no_tui: bool,

    /// Run the interactive first-use provider setup again.
    #[arg(long)]
    onboarding: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum ProviderArg {
    Auto,
    #[value(name = "openai-compatible")]
    OpenAiCompatible,
    #[value(name = "some-im")]
    SomeIm,
    Anthropic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum ApiArg {
    Auto,
    #[value(name = "chat-completions")]
    ChatCompletions,
    Responses,
    #[value(name = "anthropic-messages")]
    AnthropicMessages,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let administrative =
        cli.list_projects || cli.list_sessions || cli.list_approvals || cli.clear_approvals;
    if cli.onboarding
        || (!administrative && cli.config.is_none() && !config::default_config_path()?.exists())
    {
        onboarding::run(cli.config.as_deref()).await?;
    }
    let loaded = LoadedConfig::load(cli.config.as_deref())?;
    let home = willdeep_home()?;
    let approval_store = home.join("always-allow.json");
    if cli.list_approvals {
        for rule in load_approval_rules(&approval_store)? {
            println!("{rule}");
        }
        return Ok(());
    }
    if cli.clear_approvals {
        save_approval_rules(&approval_store, &[])?;
        println!("Cleared Always Allow rules.");
        return Ok(());
    }
    if cli.list_projects {
        for project in projects::load() {
            println!(
                "{}\t{}\t{}",
                project.id,
                project.display_name,
                project
                    .folder_paths
                    .first()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default()
            );
        }
        return Ok(());
    }
    let store = willdeep_core::SessionStore::new(&home);
    if cli.list_sessions {
        for session in store.list()? {
            println!(
                "{}\t{}\t{}\t{}",
                session.id,
                session.updated_at,
                session.workspace.display(),
                session.title
            );
        }
        return Ok(());
    }
    let resumed = load_session(&store, cli.resume.as_deref())?;
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
    let base = resolve_base(&cli, profile, selected_provider)?;
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
    let project_workspace = cli.project.as_deref().map(projects::resolve).transpose()?;
    let requested_workspace = cli
        .workspace
        .clone()
        .or(project_workspace)
        .or_else(|| resumed.as_ref().map(|session| session.workspace.clone()))
        .unwrap_or_else(|| PathBuf::from("."));
    let workspace = requested_workspace
        .canonicalize()
        .with_context(|| format!("invalid workspace: {}", requested_workspace.display()))?;
    let api_key = resolve_api_key(&cli, profile, kind)?;
    let model = cli
        .model
        .clone()
        .or_else(|| profile.and_then(|provider| provider.model.clone()))
        .or_else(|| (kind == ProviderKind::SomeIm).then(|| "deepseek-v4-flash".to_owned()))
        .context("model is required; set it in the provider profile, WILLDEEP_MODEL, or --model")?;
    let prompt = read_prompt(&cli.prompt, cli.no_tui)?;

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

    let configured_approval = loaded.file.agent.approval.as_deref().unwrap_or("strict");
    let approval_mode = if cli.full_auto {
        ApprovalMode::WorkspaceAccess
    } else {
        match configured_approval {
            "strict" | "ask" | "request-every-time" => ApprovalMode::Strict,
            "smart" | "auto-review" => ApprovalMode::Smart,
            "workspace-write" | "workspace-access" => ApprovalMode::WorkspaceAccess,
            _ => bail!("agent.approval must be `strict`, `smart`, or `workspace-write`"),
        }
    };
    let skills = Arc::new(willdeep_core::SkillCatalog::discover(
        &workspace,
        &loaded.file.skills.roots,
    ));
    let mcp = Arc::new(
        willdeep_core::McpRegistry::connect(&loaded.file.mcp_servers)
            .await
            .context("initialize MCP servers")?,
    );
    let interactive_tui = prompt.is_none() && std::io::stdin().is_terminal() && !cli.no_tui;
    let (tui_tx, tui_rx) = tui::channel();
    let relay_bridge = mobile::RelayBridge::new();
    let approver: Arc<dyn Approver> = if interactive_tui {
        Arc::new(tui::TuiApprover(tui_tx.clone()))
    } else {
        Arc::new(TerminalApprover)
    };
    let background_tasks = Arc::new(BackgroundTaskRegistry::default());
    let tools = ToolRegistry::new(&workspace, approval_mode)?
        .with_approver(approver)
        .with_skills(skills.clone())
        .with_mcp(mcp)
        .with_background_tasks(background_tasks.clone())
        .with_web_tools(web_tools)
        .with_always_allow_store(approval_store)?;
    let mut system_prompt = willdeep_core::prompt::build_system_prompt(&workspace);
    if !skills.list().is_empty() {
        system_prompt.push_str("\n\n# Available skills\nUse list_skills to search and read_skill before applying a relevant skill.\n");
        system_prompt.push_str(&skills.summary());
    }
    let sink: Arc<dyn EventSink> = if interactive_tui {
        Arc::new(tui::TuiSink {
            ui: tui_tx.clone(),
            relay: relay_bridge.clone(),
        })
    } else {
        Arc::new(TerminalSink { json: cli.json })
    };
    let context_window = profile
        .and_then(|value| value.context_window)
        .unwrap_or(128_000);
    let cheap_provider = if kind == ProviderKind::SomeIm {
        let mut cheap = parent_provider_config.clone();
        cheap.model = "glm-5".to_owned();
        build_provider(cheap).context("initialize default subagent provider")?
    } else {
        provider.clone()
    };
    let mut subagent_profiles = builtin_profiles(provider.clone(), cheap_provider, context_window);
    for subagent in &mut subagent_profiles {
        if let Some(settings) = loaded.file.subagents.get(&subagent.id) {
            if let Some(provider_name) = settings.provider_profile.as_deref() {
                let mut configured = provider_config_from_profile(&loaded.file, provider_name)?;
                if let Some(model) = &settings.model {
                    configured.model = model.clone();
                }
                subagent.provider = build_provider(configured)
                    .with_context(|| format!("initialize subagent profile {}", subagent.id))?;
            } else if let Some(model) = &settings.model {
                let mut configured = parent_provider_config.clone();
                configured.model = model.clone();
                subagent.provider = build_provider(configured)
                    .with_context(|| format!("initialize subagent profile {}", subagent.id))?;
            }
            if let Some(max_turns) = settings.max_turns {
                subagent.max_turns = max_turns;
            }
            if let Some(window) = settings.context_window {
                subagent.context_window = window;
            }
        }
    }
    let subagents = Arc::new(SubagentCatalog::new(
        &workspace,
        subagent_profiles,
        background_tasks.clone(),
    ));
    let mut agent = Agent::new(
        provider,
        tools,
        AgentConfig {
            max_turns,
            system_prompt,
            context_window,
        },
    )
    .with_event_sink(sink)
    .with_subagents(subagents);
    if let Some((vision_provider, vision_model)) = image_fallback {
        agent = agent.with_image_fallback(vision_provider, format!("some.im / {vision_model}"));
    }
    let agent = Arc::new(agent);

    let mut session = resumed.unwrap_or_else(|| {
        willdeep_core::Session::new(
            workspace.clone(),
            cli.profile.clone(),
            prompt.as_deref().unwrap_or("New session"),
        )
    });
    relay_bridge.set_session(session.id.to_string());
    if interactive_tui {
        return tui::run(
            agent,
            session,
            store,
            home,
            skills,
            relay_bridge,
            (tui_tx, tui_rx, context_window, background_tasks),
        )
        .await;
    }
    let prompt = prompt.context("provide a prompt argument or pipe one on stdin")?;
    let history = session.messages.clone();
    session.messages.push(willdeep_core::Message::user(&prompt));
    store.save(&mut session)?;
    let mut outcome = agent.run_with_history(history, prompt).await?;
    session.messages = outcome.messages.clone();
    store.save(&mut session)?;
    loop {
        let events = background_tasks.drain_pending();
        if events.is_empty() {
            let running = background_tasks
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
            if !cli.json {
                eprintln!(
                    "[background] {} finished; continuing main harness",
                    event.snapshot.id
                );
            }
            outcome = agent
                .run_with_history(session.messages.clone(), event.notice)
                .await?;
            session.messages = outcome.messages.clone();
            store.save(&mut session)?;
        }
    }
    if cli.json {
        println!(
            "{}",
            serde_json::json!({
                "type": "completed",
                "turns": outcome.turns,
                "text": outcome.final_text
            })
        );
    } else {
        println!("{}", outcome.final_text);
    }
    Ok(())
}

fn model_accepts_images(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    if lower.contains("deepseek") && !lower.contains("vl") {
        return false;
    }
    !lower.starts_with("someim-auto") && !lower.starts_with("someim-coding")
}

fn resolve_base(
    cli: &Cli,
    profile: Option<&ProviderProfile>,
    selected_provider: Option<ProviderArg>,
) -> Result<String> {
    if let Some(base) = cli
        .api_base
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(base.to_owned());
    }
    if let Some(base) = profile
        .and_then(|provider| provider.api_base.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(base.to_owned());
    }
    match selected_provider {
        Some(ProviderArg::SomeIm) => Ok("https://some.im/v1".to_owned()),
        Some(ProviderArg::Anthropic) => Ok("https://api.anthropic.com".to_owned()),
        _ => bail!(
            "API base is required; set it in the provider profile, WILLDEEP_API_BASE, or --api-base"
        ),
    }
}

fn provider_config_from_profile(file: &config::ConfigFile, name: &str) -> Result<ProviderConfig> {
    let profile = file
        .providers
        .get(name)
        .with_context(|| format!("provider profile not found: {name}"))?;
    let provider_arg = profile
        .provider
        .as_deref()
        .map(parse_provider)
        .transpose()?
        .unwrap_or(ProviderArg::Auto);
    let base = profile
        .api_base
        .clone()
        .or_else(|| match provider_arg {
            ProviderArg::SomeIm => Some("https://some.im/v1".to_owned()),
            ProviderArg::Anthropic => Some("https://api.anthropic.com".to_owned()),
            _ => None,
        })
        .with_context(|| format!("providers.{name}.api_base is required"))?;
    let kind = resolve_provider(provider_arg, &base);
    let api = profile
        .api
        .as_deref()
        .map(parse_api)
        .transpose()?
        .unwrap_or(ApiArg::Auto);
    let key = profile
        .api_key
        .clone()
        .or_else(|| {
            profile
                .api_key_env
                .as_deref()
                .and_then(|variable| std::env::var(variable).ok())
        })
        .or_else(|| match kind {
            ProviderKind::SomeIm => std::env::var("SOMEIM_API_KEY").ok(),
            ProviderKind::Anthropic => std::env::var("ANTHROPIC_API_KEY").ok(),
            ProviderKind::OpenAiCompatible => std::env::var("OPENAI_API_KEY").ok(),
        })
        .with_context(|| format!("API key for providers.{name} is unavailable"))?;
    let model = profile
        .model
        .clone()
        .with_context(|| format!("providers.{name}.model is required"))?;
    let mut configured = ProviderConfig::new(kind, resolve_dialect(api, kind), base, key, model);
    configured.max_output_tokens = profile.max_output_tokens.unwrap_or(16_384);
    Ok(configured)
}

fn resolve_provider(provider: ProviderArg, base: &str) -> ProviderKind {
    match provider {
        ProviderArg::Auto => ProviderKind::infer(base),
        ProviderArg::OpenAiCompatible => ProviderKind::OpenAiCompatible,
        ProviderArg::SomeIm => ProviderKind::SomeIm,
        ProviderArg::Anthropic => ProviderKind::Anthropic,
    }
}

fn resolve_dialect(api: ApiArg, provider: ProviderKind) -> ApiDialect {
    match api {
        ApiArg::Auto if provider == ProviderKind::Anthropic => ApiDialect::AnthropicMessages,
        ApiArg::Auto => ApiDialect::ChatCompletions,
        ApiArg::ChatCompletions => ApiDialect::ChatCompletions,
        ApiArg::Responses => ApiDialect::Responses,
        ApiArg::AnthropicMessages => ApiDialect::AnthropicMessages,
    }
}

fn resolve_api_key(
    cli: &Cli,
    profile: Option<&ProviderProfile>,
    provider: ProviderKind,
) -> Result<String> {
    let key = cli
        .api_key
        .clone()
        .or_else(|| profile.and_then(|provider| provider.api_key.clone()))
        .or_else(|| {
            profile
                .and_then(|provider| provider.api_key_env.as_deref())
                .and_then(|name| std::env::var(name).ok())
        })
        .or_else(|| {
            if provider == ProviderKind::SomeIm {
                std::env::var("SOMEIM_API_KEY").ok()
            } else if provider == ProviderKind::Anthropic {
                std::env::var("ANTHROPIC_API_KEY").ok()
            } else {
                std::env::var("OPENAI_API_KEY").ok()
            }
        });
    key.filter(|value| !value.trim().is_empty()).context(
        "API key is required; set api_key/api_key_env in the provider profile, WILLDEEP_API_KEY, a provider-specific environment variable, or --api-key",
    )
}

fn parse_provider(value: &str) -> Result<ProviderArg> {
    match value.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(ProviderArg::Auto),
        "openai-compatible" => Ok(ProviderArg::OpenAiCompatible),
        "some-im" | "some.im" => Ok(ProviderArg::SomeIm),
        "anthropic" => Ok(ProviderArg::Anthropic),
        _ => bail!("invalid provider value in config: {value}"),
    }
}

fn parse_api(value: &str) -> Result<ApiArg> {
    match value.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(ApiArg::Auto),
        "chat-completions" => Ok(ApiArg::ChatCompletions),
        "responses" => Ok(ApiArg::Responses),
        "anthropic-messages" => Ok(ApiArg::AnthropicMessages),
        _ => bail!("invalid api value in config: {value}"),
    }
}

fn read_prompt(arguments: &[String], no_tui: bool) -> Result<Option<String>> {
    if !arguments.is_empty() {
        return Ok(Some(arguments.join(" ")));
    }
    if std::io::stdin().is_terminal() {
        if no_tui {
            bail!("provide a prompt argument or pipe one on stdin");
        }
        return Ok(None);
    }
    let mut prompt = String::new();
    std::io::stdin().read_to_string(&mut prompt)?;
    if prompt.trim().is_empty() {
        bail!("prompt is empty");
    }
    Ok(Some(prompt))
}

fn load_session(
    store: &willdeep_core::SessionStore,
    resume: Option<&str>,
) -> Result<Option<willdeep_core::Session>> {
    match resume {
        Some("latest") => Ok(Some(store.latest()?.context("no saved sessions found")?)),
        Some(value) => Ok(Some(store.load(
            uuid::Uuid::parse_str(value).context("--resume must be a session UUID or `latest`")?,
        )?)),
        None => Ok(None),
    }
}

fn load_approval_rules(path: &std::path::Path) -> Result<Vec<String>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    serde_json::from_str(&std::fs::read_to_string(path)?).context("parse Always Allow rules")
}

fn save_approval_rules(path: &std::path::Path, rules: &[String]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(&serde_json::to_vec_pretty(rules)?)?;
    file.flush()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

struct TerminalApprover;

#[async_trait]
impl Approver for TerminalApprover {
    async fn approve(&self, description: &str, always_allow_available: bool) -> ApprovalDecision {
        if !std::io::stdin().is_terminal() {
            return ApprovalDecision::Deny;
        }
        let description = description.to_owned();
        tokio::task::spawn_blocking(move || {
            eprint!(
                "\nApproval required: {description}\n{} ",
                if always_allow_available {
                    "[y] Allow once / [a] Always allow / [N] Disallow"
                } else {
                    "[y] Allow once / [N] Disallow"
                }
            );
            let _ = std::io::stderr().flush();
            let mut answer = String::new();
            if std::io::stdin().read_line(&mut answer).is_err() {
                return ApprovalDecision::Deny;
            }
            match answer.trim().to_ascii_lowercase().as_str() {
                "y" | "yes" => ApprovalDecision::AllowOnce,
                "a" | "always" if always_allow_available => ApprovalDecision::AlwaysAllow,
                _ => ApprovalDecision::Deny,
            }
        })
        .await
        .unwrap_or(ApprovalDecision::Deny)
    }
    async fn ask_user(&self, request: UserQuestion) -> Option<String> {
        if !std::io::stdin().is_terminal() {
            return None;
        }
        tokio::task::spawn_blocking(move || {
            eprintln!("\n{}", request.question);
            for (index, option) in request.options.iter().enumerate() {
                eprintln!("  {}. {}", index + 1, option);
            }
            eprint!(
                "{}: ",
                if request.multi_select {
                    "Choose numbers separated by commas, or type another answer"
                } else {
                    "Choose a number, or type another answer"
                }
            );
            let _ = std::io::stderr().flush();
            let mut answer = String::new();
            std::io::stdin().read_line(&mut answer).ok()?;
            let answer = answer.trim();
            let selected = answer
                .split(',')
                .map(str::trim)
                .map(str::parse::<usize>)
                .collect::<Result<Vec<_>, _>>()
                .ok()
                .filter(|values| request.multi_select || values.len() == 1)
                .map(|values| {
                    values
                        .into_iter()
                        .filter_map(|index| request.options.get(index.saturating_sub(1)).cloned())
                        .collect::<Vec<_>>()
                })
                .filter(|values| !values.is_empty());
            Some(
                selected
                    .map(|values| values.join(", "))
                    .unwrap_or_else(|| answer.to_owned()),
            )
        })
        .await
        .unwrap_or(None)
    }
}

struct TerminalSink {
    json: bool,
}

#[async_trait]
impl EventSink for TerminalSink {
    async fn emit(&self, event: AgentEvent) {
        if self.json {
            let value = match event {
                AgentEvent::TurnStarted { turn } => {
                    serde_json::json!({"type": "turn_started", "turn": turn})
                }
                AgentEvent::AssistantText(text) => {
                    serde_json::json!({"type": "assistant_text", "text": text})
                }
                AgentEvent::ToolRequested(call) => serde_json::json!({
                    "type": "tool_requested",
                    "id": call.id,
                    "name": call.name,
                    "arguments": call.arguments
                }),
                AgentEvent::ToolCompleted {
                    call,
                    output,
                    is_error,
                } => serde_json::json!({
                    "type": "tool_completed",
                    "id": call.id,
                    "name": call.name,
                    "output": output,
                    "is_error": is_error
                }),
                AgentEvent::Usage(usage) => serde_json::json!({
                    "type": "usage",
                    "input_tokens": usage.input_tokens,
                    "output_tokens": usage.output_tokens,
                    "total_tokens": usage.total_tokens
                }),
                AgentEvent::CompressionStarted { estimated_tokens } => serde_json::json!({
                    "type": "compression_started",
                    "estimated_tokens": estimated_tokens
                }),
                AgentEvent::CompressionCompleted { estimated_tokens } => serde_json::json!({
                    "type": "compression_completed",
                    "estimated_tokens": estimated_tokens
                }),
            };
            println!("{value}");
            return;
        }
        match event {
            AgentEvent::TurnStarted { turn } if turn > 1 => eprintln!("[turn {turn}]"),
            AgentEvent::ToolRequested(call) => eprintln!("[tool] {}", call.name),
            AgentEvent::ToolCompleted {
                call,
                output,
                is_error,
            } => {
                let status = if is_error { "error" } else { "done" };
                eprintln!("[tool:{status}] {}\n{}", call.name, compact_output(&output));
            }
            AgentEvent::Usage(usage) => {
                if let Some(total) = usage.total_tokens {
                    eprintln!("[usage] {total} tokens");
                }
            }
            AgentEvent::CompressionStarted { estimated_tokens } => {
                eprintln!("[context] compressing approximately {estimated_tokens} tokens");
            }
            AgentEvent::CompressionCompleted { estimated_tokens } => {
                eprintln!("[context] compressed to approximately {estimated_tokens} tokens");
            }
            AgentEvent::AssistantText(_) | AgentEvent::TurnStarted { .. } => {}
        }
    }
}

fn compact_output(output: &str) -> String {
    const LIMIT: usize = 2_000;
    let mut value = output.chars().take(LIMIT).collect::<String>();
    if output.chars().count() > LIMIT {
        value.push_str("\n[display truncated]");
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_api_selection_matches_provider() {
        assert_eq!(
            resolve_dialect(ApiArg::Auto, ProviderKind::Anthropic),
            ApiDialect::AnthropicMessages
        );
        assert_eq!(
            resolve_dialect(ApiArg::Auto, ProviderKind::SomeIm),
            ApiDialect::ChatCompletions
        );
    }

    #[test]
    fn explicit_api_overrides_provider_default() {
        assert_eq!(
            resolve_dialect(ApiArg::Responses, ProviderKind::SomeIm),
            ApiDialect::Responses
        );
    }
}
