use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use base64::Engine;
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use serde::Deserialize;
use willdeep_core::provider::{ApiDialect, ProviderConfig, ProviderKind};
use willdeep_core::{
    AgentEvent, ApprovalDecision, Approver, EventSink, SubagentLifecycleStatus, UserQuestion,
};

mod config;
mod daemon;
mod doctor;
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
    #[arg(long, env = "WILLDEEP_CONFIG", global = true)]
    config: Option<PathBuf>,

    /// Provider profile name from the TOML configuration.
    #[arg(long, global = true)]
    profile: Option<String>,

    /// Provider API base, for example https://some.im/v1.
    #[arg(long, env = "WILLDEEP_API_BASE", global = true)]
    api_base: Option<String>,

    /// Provider API key. Prefer the environment variable to shell history.
    #[arg(long, env = "WILLDEEP_API_KEY", hide_env_values = true, global = true)]
    api_key: Option<String>,

    /// Model identifier sent to the provider.
    #[arg(long, env = "WILLDEEP_MODEL", global = true)]
    model: Option<String>,

    /// Provider identity controls authentication and some.im context headers.
    #[arg(long, value_enum, global = true)]
    provider: Option<ProviderArg>,

    /// Wire API dialect. Auto selects Anthropic Messages only for api.anthropic.com; otherwise Chat Completions.
    #[arg(long = "api", value_enum, global = true)]
    api: Option<ApiArg>,

    /// Workspace root available to tools.
    #[arg(long, global = true)]
    workspace: Option<PathBuf>,

    /// Additional workspace allowed in Web mode. May be repeated.
    #[arg(long = "web-workspace")]
    web_workspaces: Vec<PathBuf>,

    /// Allow create/edit inside the workspace without approval. Shell and MCP still ask.
    #[arg(long, global = true)]
    full_auto: bool,

    /// Maximum model/tool rounds.
    #[arg(long, global = true)]
    max_turns: Option<usize>,

    /// Maximum output tokens for Anthropic Messages.
    #[arg(long, global = true)]
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
    #[arg(long, env = "WILLDEEP_LANGUAGE", global = true)]
    language: Option<String>,

    /// Read a trusted Web bridge prompt and attachments as JSON from stdin.
    #[arg(long, hide = true)]
    web_input_json: bool,
}

#[derive(Clone, Debug, Subcommand)]
enum CliCommand {
    /// Run one non-interactive coding-agent turn.
    Run(RunArgs),
    /// Generate a completion script from the current command tree.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Render the current command tree as a roff man page.
    Man,
    /// Inspect or stop persistent Runtime Sessions.
    Session {
        #[command(subcommand)]
        action: daemon::SessionAction,
    },
    /// Create, validate, or inspect the TOML configuration.
    Config {
        #[command(subcommand)]
        action: config::ConfigAction,
    },
    /// Manage the persistent local Runtime Daemon.
    Daemon {
        #[command(subcommand)]
        action: daemon::DaemonAction,
    },
    /// Invoke one stable Runtime operation with a JSON request envelope.
    Api {
        /// Namespaced operation such as session.list or agent.get.
        operation: String,
        /// JSON object file, or - for stdin. Omit for an empty object.
        #[arg(long, value_name = "PATH|-")]
        params_file: Option<PathBuf>,
        /// Stable client-generated request ID. A UUID is generated when omitted.
        #[arg(long)]
        request_id: Option<uuid::Uuid>,
        /// Emit one compact JSON object suitable for NDJSON pipelines.
        #[arg(long)]
        ndjson: bool,
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
    /// Diagnose local configuration and runtime readiness without contacting a Provider.
    Doctor {
        /// Emit one stable JSON report.
        #[arg(long)]
        json: bool,
        /// Write a private, shareable ZIP diagnostic bundle without logs or local paths.
        #[arg(long, value_name = "PATH")]
        bundle: Option<PathBuf>,
    },
}

#[derive(Clone, Debug, Args)]
struct RunArgs {
    /// Task for the agent. Reads stdin when omitted.
    #[arg(value_name = "PROMPT", num_args = 0.., trailing_var_arg = true)]
    prompt: Vec<String>,

    /// Read the task from a UTF-8 file, or - for stdin.
    #[arg(long, value_name = "PATH|-", conflicts_with = "prompt")]
    input: Option<PathBuf>,

    /// Attach a text or PNG/JPEG/WebP/GIF file. May be repeated.
    #[arg(long, value_name = "PATH")]
    attachment: Vec<PathBuf>,

    /// Resume a saved Session by UUID or `latest`.
    #[arg(long, value_name = "ID|latest")]
    session: Option<String>,

    /// Output the final result as text, one JSON object, or NDJSON events.
    #[arg(long, value_enum, default_value = "text")]
    output: RunOutput,

    /// Suppress successful output. Errors still use stderr and a non-zero exit code.
    #[arg(long)]
    quiet: bool,

    /// Use the legacy in-process Harness instead of the persistent Runtime.
    #[arg(long)]
    local: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum RunOutput {
    Text,
    Json,
    Ndjson,
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
        std::process::exit(stable_exit_code(&error));
    }
}

#[derive(Debug)]
struct RunInputError(String);

#[derive(Debug)]
struct HeadlessRuntimeExecutionError(daemon::HeadlessRuntimeStatus);

impl std::fmt::Display for HeadlessRuntimeExecutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self.0 {
            daemon::HeadlessRuntimeStatus::WaitingApproval => {
                "Runtime Turn is waiting for approval; resolve it with `willdeep daemon pending`"
            }
            daemon::HeadlessRuntimeStatus::WaitingAnswer => {
                "Runtime Turn is waiting for an answer; resolve it with `willdeep daemon pending`"
            }
            daemon::HeadlessRuntimeStatus::Failed(_) => "Runtime Turn failed",
            daemon::HeadlessRuntimeStatus::Cancelled => "Runtime Turn was cancelled",
            daemon::HeadlessRuntimeStatus::Interrupted => "Runtime Turn was interrupted",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for HeadlessRuntimeExecutionError {}

impl std::fmt::Display for RunInputError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for RunInputError {}

fn invalid_run_input(message: impl Into<String>) -> anyhow::Error {
    RunInputError(message.into()).into()
}

fn stable_exit_code(error: &anyhow::Error) -> i32 {
    if error.downcast_ref::<RunInputError>().is_some() {
        return 2;
    }
    if error
        .downcast_ref::<willdeep_core::provider::ProviderError>()
        .is_some()
    {
        return 3;
    }
    if let Some(error) = error.downcast_ref::<willdeep_core::AgentError>() {
        return match error {
            willdeep_core::AgentError::Provider(_) => 3,
            willdeep_core::AgentError::Tool(willdeep_core::tools::ToolError::ApprovalDenied(_))
            | willdeep_core::AgentError::Tool(willdeep_core::tools::ToolError::ReadOnlyPolicy(_))
            | willdeep_core::AgentError::Tool(willdeep_core::tools::ToolError::OutsideWorkspace(
                _,
            )) => 4,
            _ => 5,
        };
    }
    if let Some(error) = error.downcast_ref::<willdeep_core::tools::ToolError>() {
        return match error {
            willdeep_core::tools::ToolError::ApprovalDenied(_)
            | willdeep_core::tools::ToolError::ReadOnlyPolicy(_)
            | willdeep_core::tools::ToolError::OutsideWorkspace(_) => 4,
            _ => 5,
        };
    }
    if let Some(error) = error.downcast_ref::<HeadlessRuntimeExecutionError>() {
        return match error.0 {
            daemon::HeadlessRuntimeStatus::WaitingApproval
            | daemon::HeadlessRuntimeStatus::WaitingAnswer
            | daemon::HeadlessRuntimeStatus::Failed(Some(
                willdeep_runtime_protocol::FailureDomain::Policy,
            )) => 4,
            daemon::HeadlessRuntimeStatus::Failed(Some(
                willdeep_runtime_protocol::FailureDomain::Provider,
            )) => 3,
            _ => 5,
        };
    }
    1
}

fn runtime_failure_domain(error: &anyhow::Error) -> willdeep_runtime_protocol::FailureDomain {
    use willdeep_runtime_protocol::FailureDomain;

    if error
        .downcast_ref::<willdeep_core::provider::ProviderError>()
        .is_some()
    {
        return FailureDomain::Provider;
    }
    if let Some(error) = error.downcast_ref::<willdeep_core::AgentError>() {
        return match error {
            willdeep_core::AgentError::Provider(_) => FailureDomain::Provider,
            willdeep_core::AgentError::Tool(willdeep_core::tools::ToolError::ApprovalDenied(_))
            | willdeep_core::AgentError::Tool(willdeep_core::tools::ToolError::ReadOnlyPolicy(_))
            | willdeep_core::AgentError::Tool(willdeep_core::tools::ToolError::OutsideWorkspace(
                _,
            )) => FailureDomain::Policy,
            willdeep_core::AgentError::Tool(_) => FailureDomain::Tool,
            _ => FailureDomain::Harness,
        };
    }
    if let Some(error) = error.downcast_ref::<willdeep_core::tools::ToolError>() {
        return match error {
            willdeep_core::tools::ToolError::ApprovalDenied(_)
            | willdeep_core::tools::ToolError::ReadOnlyPolicy(_)
            | willdeep_core::tools::ToolError::OutsideWorkspace(_) => FailureDomain::Policy,
            _ => FailureDomain::Tool,
        };
    }
    FailureDomain::Internal
}

async fn run() -> Result<()> {
    let mut cli = Cli::parse();
    let run_args = match cli.command.take() {
        Some(CliCommand::Run(args)) => {
            if args.quiet && args.output != RunOutput::Text {
                return Err(invalid_run_input(
                    "--quiet cannot be combined with --output json or ndjson",
                ));
            }
            cli.no_tui = true;
            cli.resume = args.session.clone().or(cli.resume);
            cli.json = args.output == RunOutput::Ndjson;
            Some(args)
        }
        command => {
            cli.command = command;
            None
        }
    };
    if let Some(command) = cli.command.clone() {
        return match command {
            CliCommand::Run(_) => unreachable!("run command is normalized above"),
            CliCommand::Completions { shell } => generate_completions(shell),
            CliCommand::Man => generate_man_page(),
            CliCommand::Session { action } => daemon::handle_session(action).await,
            CliCommand::Config { action } => config::handle(action, cli.config.as_deref()),
            CliCommand::Daemon { action } => daemon::handle(action).await,
            CliCommand::Api {
                operation,
                params_file,
                request_id,
                ndjson,
            } => daemon::api(operation, params_file, request_id, ndjson).await,
            CliCommand::Attach { after } => daemon::attach(after).await,
            CliCommand::Detach => daemon::detach().await,
            CliCommand::Integrations { action } => integrations::handle(action).await,
            CliCommand::Doctor { json, bundle } => {
                doctor::run(doctor::DoctorOptions {
                    config_path: cli.config.clone(),
                    profile: cli.profile.clone(),
                    workspace: cli.workspace.clone(),
                    home: willdeep_home()?,
                    api_base_present: cli
                        .api_base
                        .as_deref()
                        .is_some_and(|value| !value.is_empty()),
                    api_key_present: cli
                        .api_key
                        .as_deref()
                        .is_some_and(|value| !value.is_empty()),
                    model_present: cli.model.as_deref().is_some_and(|value| !value.is_empty()),
                    json,
                    bundle,
                })
                .await
            }
        };
    }
    let administrative =
        cli.list_projects || cli.list_sessions || cli.list_approvals || cli.clear_approvals;
    if cli.onboarding
        || (!cli.web
            && !administrative
            && cli.config.is_none()
            && !config::default_config_path()?.exists())
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
    let resumed = load_session(&store, cli.resume.as_deref()).map_err(|error| {
        if run_args.is_some() {
            invalid_run_input(format!("load run Session: {error:#}"))
        } else {
            error
        }
    })?;
    let web_input = if cli.web_input_json {
        let mut value = String::new();
        std::io::stdin().read_to_string(&mut value)?;
        Some(serde_json::from_str::<WebInput>(&value).context("parse Web bridge input")?)
    } else {
        None
    };
    let prompt = if let Some(input) = &web_input {
        Some(input.prompt.clone())
    } else if let Some(args) = &run_args {
        Some(read_run_prompt(args)?)
    } else {
        read_prompt(&cli.prompt, cli.no_tui)?
    };
    let run_attachments = run_args
        .as_ref()
        .map(|args| load_run_attachments(&args.attachment))
        .transpose()?
        .unwrap_or_default();

    let interactive_tui = prompt.is_none() && std::io::stdin().is_terminal() && !cli.no_tui;
    if should_use_headless_runtime(
        &cli,
        run_args.as_ref(),
        interactive_tui,
        web_input.is_some(),
    ) {
        let prompt = prompt.context("provide a prompt argument or pipe one on stdin")?;
        return run_with_runtime(
            &cli,
            &home,
            &store,
            resumed,
            prompt,
            run_attachments,
            run_args.as_ref(),
        )
        .await;
    }
    let (tui_tx, tui_rx) = tui::channel();
    let relay_bridge = mobile::RelayBridge::new();
    let frontend =
        if let Some(connection) = web_input.as_ref().and_then(|input| input.runtime.clone()) {
            harness::HarnessFrontend::Runtime {
                connection,
                sink: Arc::new(TerminalSink {
                    json: cli.json,
                    quiet: run_args
                        .as_ref()
                        .is_some_and(|args| args.quiet || args.output == RunOutput::Json),
                }),
                workspace_access: Some(daemon::WorkspaceAccess::ReadOnly),
                allowed_skills: Vec::new(),
                allowed_mcp_servers: Vec::new(),
            }
        } else if interactive_tui {
            harness::HarnessFrontend::Tui {
                tx: tui_tx.clone(),
                relay: relay_bridge.clone(),
            }
        } else {
            harness::HarnessFrontend::Terminal {
                json: cli.json,
                quiet: run_args
                    .as_ref()
                    .is_some_and(|args| args.quiet || args.output == RunOutput::Json),
            }
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
    if session.config.is_none() {
        session.config = Some(cli.config.clone().unwrap_or(config::default_config_path()?));
    }
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
    let attachments = if let Some(input) = web_input {
        input.attachments
    } else {
        run_attachments
    };
    let outcome = harness::execute_noninteractive(
        &built,
        &store,
        &mut session,
        prompt,
        attachments,
        language,
        allow_compress_command,
    )
    .await?;
    if let Some(args) = run_args {
        match args.output {
            RunOutput::Text if !args.quiet && !outcome.compressed => {
                println!("{}", outcome.final_text);
            }
            RunOutput::Json => println!("{}", completion_json(&outcome, session.id)),
            RunOutput::Ndjson => println!("{}", completion_json(&outcome, session.id)),
            RunOutput::Text => {}
        }
    } else if cli.json {
        println!("{}", completion_json(&outcome, session.id));
    } else if !outcome.compressed {
        println!("{}", outcome.final_text);
    }
    Ok(())
}

fn should_use_headless_runtime(
    cli: &Cli,
    run_args: Option<&RunArgs>,
    interactive_tui: bool,
    web_bridge: bool,
) -> bool {
    if interactive_tui || web_bridge || run_args.is_some_and(|args| args.local) {
        return false;
    }
    // These process-local overrides are intentionally not serialized into the
    // Runtime task store. Use TOML profiles for persistent Runtime execution,
    // or --local when an ephemeral override is required.
    cli.api_base.is_none()
        && cli.api_key.is_none()
        && cli.provider.is_none()
        && cli.api.is_none()
        && !cli.full_auto
        && cli.max_turns.is_none()
        && cli.max_output_tokens.is_none()
        && cli.language.is_none()
}

async fn run_with_runtime(
    cli: &Cli,
    home: &std::path::Path,
    store: &willdeep_core::SessionStore,
    resumed: Option<willdeep_core::Session>,
    prompt: String,
    attachments: Vec<willdeep_core::MessageAttachment>,
    run_args: Option<&RunArgs>,
) -> Result<()> {
    let workspace = harness::resolve_workspace(cli, resumed.as_ref())?;
    let mut session = resumed.unwrap_or_else(|| {
        willdeep_core::Session::new(workspace.clone(), cli.profile.clone(), &prompt)
    });
    if session.config.is_none() {
        session.config = Some(cli.config.clone().unwrap_or(config::default_config_path()?));
    }
    session.runtime_managed = true;
    store.save(&mut session)?;

    let quiet = run_args.is_some_and(|args| args.quiet || args.output == RunOutput::Json);
    let machine_events = cli.json || run_args.is_some_and(|args| args.output == RunOutput::Ndjson);
    let result = daemon::execute_headless_turn(
        home,
        daemon::HeadlessRuntimeRequest {
            session_id: session.id,
            workspace,
            profile: session.profile.clone().or_else(|| cli.profile.clone()),
            model: cli.model.clone(),
            title: session.title.clone(),
            prompt,
            attachments,
        },
        |event| emit_headless_runtime_event(event, machine_events, quiet),
    )
    .await?;
    let outcome =
        result.map_err(|status| anyhow::Error::new(HeadlessRuntimeExecutionError(status)))?;
    if let Some(args) = run_args {
        match args.output {
            RunOutput::Text if !args.quiet => println!("{}", outcome.final_text),
            RunOutput::Json | RunOutput::Ndjson => println!(
                "{}",
                completion_json_values(&outcome.final_text, outcome.turns, outcome.session_id)
            ),
            RunOutput::Text => {}
        }
    } else if cli.json {
        println!(
            "{}",
            completion_json_values(&outcome.final_text, outcome.turns, outcome.session_id)
        );
    } else {
        println!("{}", outcome.final_text);
    }
    Ok(())
}

fn emit_headless_runtime_event(event: serde_json::Value, machine: bool, quiet: bool) {
    if quiet {
        return;
    }
    if machine {
        println!("{event}");
        return;
    }
    let kind = event
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("runtime");
    match kind {
        "turn_started" => {
            if let Some(turn) = event.get("turn").and_then(serde_json::Value::as_u64) {
                eprintln!("[turn {turn}]");
            }
        }
        "tool_requested" => eprintln!(
            "[tool] {}",
            event
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
        ),
        "tool_completed" => eprintln!(
            "[tool:{}] {}",
            if event
                .get("is_error")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                "error"
            } else {
                "done"
            },
            event
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
        ),
        "compression_started" => eprintln!("[context] compressing"),
        "compression_completed" => eprintln!("[context] compressed"),
        "subagent_started" => eprintln!("[subagent] started"),
        "subagent_completed" => eprintln!("[subagent] finished"),
        _ => {}
    }
}

fn generate_completions(shell: clap_complete::Shell) -> Result<()> {
    let mut command = Cli::command();
    let name = command.get_name().to_owned();
    clap_complete::generate(shell, &mut command, name, &mut std::io::stdout());
    Ok(())
}

fn generate_man_page() -> Result<()> {
    clap_mangen::Man::new(Cli::command())
        .render(&mut std::io::stdout())
        .context("render willdeep man page")
}

fn completion_json(outcome: &harness::HarnessOutcome, session_id: uuid::Uuid) -> serde_json::Value {
    completion_json_values(&outcome.final_text, outcome.turns, session_id)
}

fn completion_json_values(
    final_text: &str,
    turns: usize,
    session_id: uuid::Uuid,
) -> serde_json::Value {
    serde_json::json!({
        "type": "completed",
        "turns": turns,
        "text": final_text,
        "session_id": session_id
    })
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

fn read_run_prompt(args: &RunArgs) -> Result<String> {
    if !args.prompt.is_empty() {
        return Ok(args.prompt.join(" "));
    }
    let mut prompt = String::new();
    match args.input.as_deref() {
        Some(path) if path == std::path::Path::new("-") => {
            std::io::stdin().read_to_string(&mut prompt)?;
        }
        Some(path) => {
            prompt = std::fs::read_to_string(path).map_err(|error| {
                invalid_run_input(format!("read run input {}: {error}", path.display()))
            })?;
        }
        None if std::io::stdin().is_terminal() => {
            return Err(invalid_run_input(
                "provide a prompt, --input PATH, or pipe a prompt on stdin",
            ));
        }
        None => {
            std::io::stdin().read_to_string(&mut prompt)?;
        }
    }
    if prompt.trim().is_empty() {
        return Err(invalid_run_input("prompt is empty"));
    }
    Ok(prompt)
}

fn load_run_attachments(paths: &[PathBuf]) -> Result<Vec<willdeep_core::MessageAttachment>> {
    const MAX_ATTACHMENTS: usize = 12;
    const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
    const MAX_TEXT_CHARS: usize = 200_000;
    const MAX_IMAGE_PIXELS: u64 = 100_000_000;

    if paths.len() > MAX_ATTACHMENTS {
        return Err(invalid_run_input(format!(
            "at most {MAX_ATTACHMENTS} attachments are allowed"
        )));
    }
    let mut attachments = Vec::with_capacity(paths.len());
    let mut total_bytes = 0usize;
    for path in paths {
        let bytes = std::fs::read(path).map_err(|error| {
            invalid_run_input(format!("read attachment {}: {error}", path.display()))
        })?;
        total_bytes = total_bytes.saturating_add(bytes.len());
        if bytes.is_empty() || total_bytes > MAX_ATTACHMENT_BYTES {
            return Err(invalid_run_input(
                "attachments must be non-empty and total at most 10 MiB",
            ));
        }
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| invalid_run_input("attachment file name must be valid UTF-8"))?
            .to_owned();
        if let Ok(format) = image::guess_format(&bytes) {
            let media_type = match format {
                image::ImageFormat::Png => "image/png",
                image::ImageFormat::Jpeg => "image/jpeg",
                image::ImageFormat::WebP => "image/webp",
                image::ImageFormat::Gif => "image/gif",
                _ => {
                    return Err(invalid_run_input(format!(
                        "unsupported image attachment format: {}",
                        path.display()
                    )));
                }
            };
            let (width, height) = image::ImageReader::new(std::io::Cursor::new(&bytes))
                .with_guessed_format()
                .map_err(|error| {
                    invalid_run_input(format!("detect attachment image format: {error}"))
                })?
                .into_dimensions()
                .map_err(|error| {
                    invalid_run_input(format!("read image dimensions {}: {error}", path.display()))
                })?;
            if width == 0
                || height == 0
                || u64::from(width).saturating_mul(u64::from(height)) > MAX_IMAGE_PIXELS
            {
                return Err(invalid_run_input(
                    "image attachment dimensions are invalid or too large",
                ));
            }
            attachments.push(willdeep_core::MessageAttachment::Image {
                name,
                media_type: media_type.to_owned(),
                data: base64::engine::general_purpose::STANDARD.encode(bytes),
                width,
                height,
            });
        } else {
            let content = String::from_utf8(bytes).map_err(|_| {
                invalid_run_input(format!(
                    "attachment is neither supported image nor UTF-8 text: {}",
                    path.display()
                ))
            })?;
            if content.chars().count() > MAX_TEXT_CHARS {
                return Err(invalid_run_input(format!(
                    "text attachment exceeds {MAX_TEXT_CHARS} characters"
                )));
            }
            attachments.push(willdeep_core::MessageAttachment::Text { name, content });
        }
    }
    Ok(attachments)
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
    quiet: bool,
}

#[async_trait]
impl EventSink for TerminalSink {
    async fn emit(&self, event: AgentEvent) {
        if self.quiet {
            return;
        }
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
            "name": call.name
        }),
        AgentEvent::ToolCompleted {
            call,
            output: _,
            is_error,
        } => serde_json::json!({
            "type": "tool_completed",
            "id": call.id,
            "name": call.name,
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
            workspace: _,
            root_workspace: _,
            worktree_branch,
            dedicated_worktree,
        } => serde_json::json!({
            "type": "subagent_started",
            "id": id,
            "profile": profile,
            "label": label,
            "background": background,
            "max_turns": max_turns,
            "token_budget": token_budget,
            "timeout_seconds": timeout_seconds,
            "worktree_branch": worktree_branch,
            "dedicated_worktree": dedicated_worktree
        }),
        AgentEvent::SubagentCompleted {
            id,
            status,
            report: _,
        } => serde_json::json!({
            "type": "subagent_completed",
            "id": id,
            "status": match status {
                SubagentLifecycleStatus::Completed => "completed",
                SubagentLifecycleStatus::Blocked => "blocked",
                SubagentLifecycleStatus::Cancelled => "cancelled",
                SubagentLifecycleStatus::Failed => "failed",
            }
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
    fn run_subcommand_accepts_global_options_and_explicit_output() {
        let cli = Cli::try_parse_from([
            "willdeep",
            "run",
            "--profile",
            "coding",
            "--workspace",
            ".",
            "--output",
            "json",
            "inspect",
            "this",
        ])
        .unwrap();
        assert_eq!(cli.profile.as_deref(), Some("coding"));
        assert_eq!(cli.workspace.as_deref(), Some(std::path::Path::new(".")));
        let Some(CliCommand::Run(args)) = cli.command else {
            panic!("expected run command");
        };
        assert_eq!(args.prompt, ["inspect", "this"]);
        assert_eq!(args.output, RunOutput::Json);
    }

    #[test]
    fn run_subcommand_rejects_prompt_and_input_together() {
        assert!(
            Cli::try_parse_from(["willdeep", "run", "--input", "prompt.txt", "inline"]).is_err()
        );
    }

    #[test]
    fn headless_run_defaults_to_runtime_and_keeps_secret_overrides_local() {
        let runtime = Cli::try_parse_from(["willdeep", "run", "inspect"]).unwrap();
        let Some(CliCommand::Run(runtime_args)) = runtime.command.as_ref() else {
            panic!("run command");
        };
        assert!(should_use_headless_runtime(
            &runtime,
            Some(runtime_args),
            false,
            false
        ));

        let local = Cli::try_parse_from(["willdeep", "run", "--local", "inspect"]).unwrap();
        let Some(CliCommand::Run(local_args)) = local.command.as_ref() else {
            panic!("run command");
        };
        assert!(!should_use_headless_runtime(
            &local,
            Some(local_args),
            false,
            false
        ));

        let secret =
            Cli::try_parse_from(["willdeep", "run", "--api-key", "not-serialized", "inspect"])
                .unwrap();
        let Some(CliCommand::Run(secret_args)) = secret.command.as_ref() else {
            panic!("run command");
        };
        assert!(!should_use_headless_runtime(
            &secret,
            Some(secret_args),
            false,
            false
        ));
    }

    #[test]
    fn top_level_session_commands_parse_stable_targets() {
        let id = uuid::Uuid::new_v4();
        for action in ["get", "turns", "stop"] {
            let cli =
                Cli::try_parse_from(["willdeep", "session", action, &id.to_string()]).unwrap();
            assert!(matches!(cli.command, Some(CliCommand::Session { .. })));
        }
        let cli = Cli::try_parse_from(["willdeep", "session", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(CliCommand::Session {
                action: daemon::SessionAction::List
            })
        ));
    }

    #[test]
    fn doctor_accepts_machine_output_and_global_context_options() {
        let cli = Cli::try_parse_from([
            "willdeep",
            "doctor",
            "--json",
            "--bundle",
            "diagnostics.zip",
            "--workspace",
            ".",
            "--profile",
            "coding",
        ])
        .unwrap();
        assert_eq!(cli.workspace.as_deref(), Some(std::path::Path::new(".")));
        assert_eq!(cli.profile.as_deref(), Some("coding"));
        assert!(matches!(
            cli.command,
            Some(CliCommand::Doctor {
                json: true,
                bundle: Some(ref path)
            })
            if path == std::path::Path::new("diagnostics.zip")
        ));
    }

    #[test]
    fn run_attachments_load_text_and_supported_image_metadata() {
        let root = std::env::temp_dir().join(format!("willdeep-run-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let text_path = root.join("notes.txt");
        std::fs::write(&text_path, "evidence").unwrap();
        let image_path = root.join("pixel.png");
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(2, 3)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        std::fs::write(&image_path, png.into_inner()).unwrap();

        let attachments = load_run_attachments(&[text_path, image_path]).unwrap();
        assert!(matches!(
            &attachments[0],
            willdeep_core::MessageAttachment::Text { content, .. } if content == "evidence"
        ));
        assert!(matches!(
            &attachments[1],
            willdeep_core::MessageAttachment::Image {
                media_type,
                width: 2,
                height: 3,
                ..
            } if media_type == "image/png"
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn run_exit_codes_are_stable_by_failure_domain() {
        assert_eq!(stable_exit_code(&invalid_run_input("bad input")), 2);
        assert_eq!(
            stable_exit_code(&anyhow::Error::new(
                willdeep_core::provider::ProviderError::MissingApiKey
            )),
            3
        );
        assert_eq!(
            stable_exit_code(&anyhow::Error::new(willdeep_core::AgentError::Tool(
                willdeep_core::tools::ToolError::ApprovalDenied("test".to_owned())
            ))),
            4
        );
        assert_eq!(
            stable_exit_code(&anyhow::Error::new(willdeep_core::AgentError::MaxTurns(3))),
            5
        );
        assert_eq!(stable_exit_code(&anyhow::anyhow!("unknown")), 1);
        assert_eq!(
            stable_exit_code(&anyhow::Error::new(HeadlessRuntimeExecutionError(
                daemon::HeadlessRuntimeStatus::Failed(Some(
                    willdeep_runtime_protocol::FailureDomain::Provider
                ))
            ))),
            3
        );
        assert_eq!(
            stable_exit_code(&anyhow::Error::new(HeadlessRuntimeExecutionError(
                daemon::HeadlessRuntimeStatus::WaitingApproval
            ))),
            4
        );
    }

    #[test]
    fn run_completion_json_has_one_stable_machine_readable_envelope() {
        let session_id = uuid::Uuid::new_v4();
        let value = completion_json(
            &harness::HarnessOutcome {
                final_text: "done".to_owned(),
                turns: 4,
                compressed: false,
            },
            session_id,
        );
        assert_eq!(value["type"], "completed");
        assert_eq!(value["turns"], 4);
        assert_eq!(value["text"], "done");
        assert_eq!(value["session_id"], session_id.to_string());
        assert!(value.as_object().is_some_and(|object| object.len() == 4));
    }

    #[test]
    fn every_supported_shell_generates_a_completion_script() {
        for shell in [
            clap_complete::Shell::Bash,
            clap_complete::Shell::Zsh,
            clap_complete::Shell::Fish,
            clap_complete::Shell::PowerShell,
        ] {
            let mut command = Cli::command();
            let mut output = Vec::new();
            clap_complete::generate(shell, &mut command, "willdeep", &mut output);
            let output = String::from_utf8(output).unwrap();
            assert!(output.contains("willdeep"), "missing command for {shell:?}");
            assert!(output.len() > 100, "completion too short for {shell:?}");
        }
    }

    #[test]
    fn generated_man_page_documents_run_and_runtime_commands() {
        let mut output = Vec::new();
        clap_mangen::Man::new(Cli::command())
            .render(&mut output)
            .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains(".TH willdeep"));
        assert!(output.contains("Run one non\\-interactive coding\\-agent turn"));
        assert!(output.contains("Manage the persistent local Runtime Daemon"));
        assert!(output.contains("Diagnose local configuration and runtime readiness"));
    }

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
