use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use clap::{Parser, Subcommand, ValueEnum};
use serde::Deserialize;
use willdeep_core::provider::{ApiDialect, ProviderConfig, ProviderKind};
use willdeep_core::{
    AgentEvent, ApprovalDecision, Approver, EventSink, SubagentLifecycleStatus, UserQuestion,
};

mod config;
mod daemon;
mod editor;
mod harness;
mod i18n;
mod integrations;
mod mobile;
mod onboarding;
mod projects;
mod tui;
mod web;

use config::{LoadedConfig, ProviderProfile, willdeep_home};

#[derive(Parser, Debug)]
#[command(
    name = "willdeep",
    version,
    about = "Cross-platform WillDeep coding agent"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<CliCommand>,

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

    /// Additional workspace allowed in Web mode. May be repeated.
    #[arg(long = "web-workspace")]
    web_workspaces: Vec<PathBuf>,

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

    /// Start the embedded browser UI and JSON API instead of the TUI.
    #[arg(long)]
    web: bool,

    /// Web server listen address. Authentication belongs at the reverse proxy.
    #[arg(long, default_value = "127.0.0.1:9847")]
    listen: std::net::SocketAddr,

    /// UI language: zh-CN, en, or ja. Overrides agent.language.
    #[arg(long, env = "WILLDEEP_LANGUAGE")]
    language: Option<String>,

    /// Read a trusted Web bridge prompt and attachments as JSON from stdin.
    #[arg(long, hide = true)]
    web_input_json: bool,
}

#[derive(Clone, Debug, Subcommand)]
enum CliCommand {
    /// Manage the persistent local Runtime Daemon.
    Daemon {
        #[command(subcommand)]
        action: daemon::DaemonAction,
    },
    /// Attach to the persistent Runtime event stream.
    Attach {
        /// Resume after this event sequence number.
        #[arg(long, default_value_t = 0)]
        after: u64,
    },
    /// Confirm that this client can disconnect without stopping the Runtime.
    Detach,
    /// Inspect and manage optional external integrations.
    Integrations {
        #[command(subcommand)]
        action: integrations::IntegrationAction,
    },
}

#[derive(Deserialize)]
struct WebInput {
    prompt: String,
    #[serde(default)]
    attachments: Vec<willdeep_core::MessageAttachment>,
    #[serde(default)]
    runtime: Option<daemon::RuntimeConnection>,
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
    if let Some(command) = cli.command.clone() {
        return match command {
            CliCommand::Daemon { action } => daemon::handle(action).await,
            CliCommand::Attach { after } => daemon::attach(after).await,
            CliCommand::Detach => daemon::detach().await,
            CliCommand::Integrations { action } => integrations::handle(action).await,
        };
    }
    let administrative =
        cli.list_projects || cli.list_sessions || cli.list_approvals || cli.clear_approvals;
    if cli.onboarding
        || (!administrative && cli.config.is_none() && !config::default_config_path()?.exists())
    {
        onboarding::run(cli.config.as_deref()).await?;
    }
    let loaded = LoadedConfig::load(cli.config.as_deref())?;
    let language = i18n::Language::parse(
        cli.language
            .as_deref()
            .or(loaded.file.agent.language.as_deref()),
    )?;
    let home = willdeep_home()?;
    if cli.web {
        let mut candidates = Vec::new();
        if let Some(project) = cli.project.as_deref() {
            candidates.extend(projects::resolve_folders(project)?);
        }
        if let Some(workspace) = cli.workspace.clone() {
            candidates.insert(0, workspace);
        }
        candidates.extend(cli.web_workspaces.clone());
        if candidates.is_empty() {
            candidates.push(std::env::current_dir()?);
        }
        let mut workspaces = Vec::new();
        for candidate in candidates {
            let canonical = candidate
                .canonicalize()
                .with_context(|| format!("invalid Web workspace: {}", candidate.display()))?;
            if !workspaces.contains(&canonical) {
                workspaces.push(canonical);
            }
        }
        return web::serve(web::WebConfig {
            listen: cli.listen,
            config_path: cli.config.clone().unwrap_or(config::default_config_path()?),
            profile: cli.profile.clone(),
            workspaces,
            home,
            language,
        })
        .await;
    }
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
    let web_input = if cli.web_input_json {
        let mut value = String::new();
        std::io::stdin().read_to_string(&mut value)?;
        Some(serde_json::from_str::<WebInput>(&value).context("parse Web bridge input")?)
    } else {
        None
    };
    let prompt = if let Some(input) = &web_input {
        Some(input.prompt.clone())
    } else {
        read_prompt(&cli.prompt, cli.no_tui)?
    };

    let interactive_tui = prompt.is_none() && std::io::stdin().is_terminal() && !cli.no_tui;
    let (tui_tx, tui_rx) = tui::channel();
    let relay_bridge = mobile::RelayBridge::new();
    let frontend =
        if let Some(connection) = web_input.as_ref().and_then(|input| input.runtime.clone()) {
            harness::HarnessFrontend::Runtime {
                connection,
                sink: Arc::new(TerminalSink { json: cli.json }),
                read_only: true,
                allowed_skills: Vec::new(),
                allowed_mcp_servers: Vec::new(),
            }
        } else if interactive_tui {
            harness::HarnessFrontend::Tui {
                tx: tui_tx.clone(),
                relay: relay_bridge.clone(),
            }
        } else {
            harness::HarnessFrontend::Terminal { json: cli.json }
        };
    let built = harness::build(&cli, &loaded, &home, language, resumed.as_ref(), frontend).await?;
    let agent = built.agent.clone();
    let workspace = built.workspace.clone();
    let skills = built.skills.clone();
    let background_tasks = built.background_tasks.clone();
    let context_window = built.context_window;

    let mut session = resumed.unwrap_or_else(|| {
        willdeep_core::Session::new(
            workspace.clone(),
            cli.profile.clone(),
            prompt.as_deref().unwrap_or("New session"),
        )
    });
    relay_bridge.set_session(session.id.to_string());
    if interactive_tui {
        let runtime_profile = session.profile.clone().or_else(|| cli.profile.clone());
        return tui::run(
            agent,
            session,
            store,
            home,
            skills,
            relay_bridge,
            (
                tui_tx,
                tui_rx,
                context_window,
                background_tasks,
                daemon::RuntimeSubmitOptions {
                    workspace,
                    profile: runtime_profile,
                    model: cli.model.clone(),
                    config: Some(cli.config.clone().unwrap_or(config::default_config_path()?)),
                },
                language,
            ),
        )
        .await;
    }
    let prompt = prompt.context("provide a prompt argument or pipe one on stdin")?;
    let allow_compress_command = web_input.is_some();
    let outcome = harness::execute_noninteractive(
        &built,
        &store,
        &mut session,
        prompt,
        web_input.map(|input| input.attachments).unwrap_or_default(),
        language,
        allow_compress_command,
    )
    .await?;
    if cli.json {
        println!(
            "{}",
            serde_json::json!({
                "type": "completed",
                "turns": outcome.turns,
                "text": outcome.final_text,
                "session_id": session.id
            })
        );
    } else if !outcome.compressed {
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

struct TerminalApprover(i18n::Language);

#[async_trait]
impl Approver for TerminalApprover {
    async fn approve(&self, description: &str, always_allow_available: bool) -> ApprovalDecision {
        if !std::io::stdin().is_terminal() {
            return ApprovalDecision::Deny;
        }
        let description = description.to_owned();
        let language = self.0;
        tokio::task::spawn_blocking(move || {
            eprint!(
                "\n{}: {description}\n{} ",
                language.text("需要确认", "Approval required", "承認が必要"),
                if always_allow_available {
                    language.text(
                        "[y] 允许一次 / [a] 始终允许 / [N] 拒绝",
                        "[y] Allow once / [a] Always allow / [N] Disallow",
                        "[y] 一度だけ許可 / [a] 常に許可 / [N] 拒否",
                    )
                } else {
                    language.text(
                        "[y] 允许一次 / [N] 拒绝",
                        "[y] Allow once / [N] Disallow",
                        "[y] 一度だけ許可 / [N] 拒否",
                    )
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
            let value = agent_event_json(event);
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
            AgentEvent::SubagentStarted {
                id,
                profile,
                background,
                ..
            } => eprintln!("[subagent] started id={id} profile={profile} background={background}"),
            AgentEvent::SubagentCompleted { id, status, .. } => {
                eprintln!("[subagent] finished id={id} status={status:?}")
            }
            AgentEvent::SubagentTurnStarted { id, turn } => {
                eprintln!("[subagent] id={id} turn={turn}")
            }
            AgentEvent::SubagentToolRequested { id, name } => {
                eprintln!("[subagent] id={id} tool={name}")
            }
            AgentEvent::SubagentToolCompleted { id, name, is_error } => eprintln!(
                "[subagent] id={id} tool={name} status={}",
                if is_error { "error" } else { "done" }
            ),
            AgentEvent::SubagentUsage { id, usage } => {
                if let Some(total) = usage.total_tokens {
                    eprintln!("[subagent] id={id} usage={total}");
                }
            }
            AgentEvent::AssistantText(_) | AgentEvent::TurnStarted { .. } => {}
        }
    }
}

pub(crate) fn agent_event_json(event: AgentEvent) -> serde_json::Value {
    match event {
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
        AgentEvent::SubagentStarted {
            id,
            profile,
            label,
            background,
            max_turns,
            token_budget,
            timeout_seconds,
        } => serde_json::json!({
            "type": "subagent_started",
            "id": id,
            "profile": profile,
            "label": label,
            "background": background,
            "max_turns": max_turns,
            "token_budget": token_budget,
            "timeout_seconds": timeout_seconds
        }),
        AgentEvent::SubagentCompleted { id, status, report } => serde_json::json!({
            "type": "subagent_completed",
            "id": id,
            "status": match status {
                SubagentLifecycleStatus::Completed => "completed",
                SubagentLifecycleStatus::Blocked => "blocked",
                SubagentLifecycleStatus::Cancelled => "cancelled",
                SubagentLifecycleStatus::Failed => "failed",
            },
            "report": report
        }),
        AgentEvent::SubagentTurnStarted { id, turn } => serde_json::json!({
            "type": "subagent_turn_started",
            "id": id,
            "turn": turn
        }),
        AgentEvent::SubagentToolRequested { id, name } => serde_json::json!({
            "type": "subagent_tool_requested",
            "id": id,
            "name": name
        }),
        AgentEvent::SubagentToolCompleted { id, name, is_error } => serde_json::json!({
            "type": "subagent_tool_completed",
            "id": id,
            "name": name,
            "is_error": is_error
        }),
        AgentEvent::SubagentUsage { id, usage } => serde_json::json!({
            "type": "subagent_usage",
            "id": id,
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens,
            "total_tokens": usage.total_tokens
        }),
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

    #[test]
    fn agent_event_json_has_stable_runtime_schema() {
        assert_eq!(
            agent_event_json(AgentEvent::TurnStarted { turn: 3 }),
            serde_json::json!({"type": "turn_started", "turn": 3})
        );
        assert_eq!(
            agent_event_json(AgentEvent::AssistantText("ready".to_string())),
            serde_json::json!({"type": "assistant_text", "text": "ready"})
        );
    }
}
