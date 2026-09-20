//! `willdeep mcp`：看配置里的 MCP 服务、给远程服务做 OAuth 登录、连上去列工具。
//!
//! 登录是 OAuth 2.1 授权码 + PKCE：本机开一个只监听 127.0.0.1 的随机端口收回调，
//! token 落在 `$WILLDEEP_HOME/mcp-oauth/<name>.json`（0600）。之后 Harness 连这个
//! 服务时自动带 token、到期自动刷新；刷新不了就再登一次。

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use willdeep_core::mcp::{McpRegistry, McpServerConfig, McpTransportKind, oauth};

use crate::LoadedConfig;
use crate::i18n::Language;

#[derive(Clone, Debug, Subcommand)]
pub(crate) enum McpAction {
    /// List configured MCP servers with their transport and authentication state.
    List {
        /// Emit one JSON array instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Log in to a remote MCP server with OAuth 2.1 (authorization code + PKCE, loopback redirect).
    Login {
        /// Server name, as in [mcp_servers.<name>].
        name: String,
        /// Seconds to wait for the browser to come back.
        #[arg(long, default_value_t = 300)]
        timeout: u64,
        /// Print the authorization URL only; do not try to open a browser.
        #[arg(long)]
        no_browser: bool,
    },
    /// Forget the stored OAuth tokens of a server.
    Logout {
        /// Server name, as in [mcp_servers.<name>].
        name: String,
    },
    /// Connect to one server and list the tools it exposes.
    Tools {
        /// Server name, as in [mcp_servers.<name>].
        name: String,
        /// Emit one JSON array with full input schemas.
        #[arg(long)]
        json: bool,
    },
}

pub(crate) async fn run(
    action: McpAction,
    home: &Path,
    config_path: Option<&Path>,
    language: Language,
) -> Result<()> {
    let loaded = LoadedConfig::load(config_path)?;
    let servers = &loaded.file.mcp_servers;
    match action {
        McpAction::List { json } => list(home, servers, json, language),
        McpAction::Login {
            name,
            timeout,
            no_browser,
        } => login(home, servers, &name, timeout, no_browser, language).await,
        McpAction::Logout { name } => logout(home, &name, language),
        McpAction::Tools { name, json } => tools(home, servers, &name, json, language).await,
    }
}

fn server<'a>(
    servers: &'a BTreeMap<String, McpServerConfig>,
    name: &str,
) -> Result<&'a McpServerConfig> {
    servers.get(name).with_context(|| {
        format!(
            "no [mcp_servers.{name}] in the configuration (known: {})",
            servers.keys().cloned().collect::<Vec<_>>().join(", ")
        )
    })
}

fn auth_label(home: &Path, name: &str, config: &McpServerConfig, language: Language) -> String {
    if let Some(variable) = &config.bearer_token_env {
        let present = std::env::var(variable).is_ok_and(|value| !value.trim().is_empty());
        return format!(
            "bearer ${variable} ({})",
            if present {
                language.text("已设置", "set", "設定済み")
            } else {
                language.text("未设置", "unset", "未設定")
            }
        );
    }
    if config.oauth.is_some() {
        return match oauth::load_tokens(home, name) {
            Ok(Some(tokens)) => format!(
                "oauth {}{}",
                language.text("已登录", "signed in", "ログイン済み"),
                match tokens.expires_at {
                    Some(at) if tokens.expired() => format!(
                        "，{} {}",
                        language.text("已过期于", "expired at", "期限切れ"),
                        willdeep_core::session::format_iso8601(at)
                    ),
                    Some(at) => format!(
                        "，{} {}",
                        language.text("有效期至", "valid until", "有効期限"),
                        willdeep_core::session::format_iso8601(at)
                    ),
                    None => String::new(),
                }
            ),
            Ok(None) => format!(
                "oauth {}",
                language.text("未登录", "not signed in", "未ログイン")
            ),
            Err(error) => format!("oauth ({error})"),
        };
    }
    language.text("无", "none", "なし").to_owned()
}

fn list(
    home: &Path,
    servers: &BTreeMap<String, McpServerConfig>,
    json: bool,
    language: Language,
) -> Result<()> {
    if json {
        let rows = servers
            .iter()
            .map(|(name, config)| {
                serde_json::json!({
                    "name": name,
                    "transport": transport_label(config),
                    "enabled": config.enabled,
                    "target": config.url.clone().or_else(|| config.command.clone()),
                    "auth": auth_label(home, name, config, Language::En),
                    "valid": config.validate(name).map(|_| true).unwrap_or(false),
                })
            })
            .collect::<Vec<_>>();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if servers.is_empty() {
        println!(
            "{}",
            language.text(
                "配置里没有 MCP 服务（[mcp_servers.<name>]）。",
                "No MCP servers configured ([mcp_servers.<name>]).",
                "MCP サーバーは設定されていません（[mcp_servers.<name>]）。"
            )
        );
        return Ok(());
    }
    for (name, config) in servers {
        let state = match config.validate(name) {
            Ok(()) if config.enabled => language.text("启用", "enabled", "有効").to_owned(),
            Ok(()) => language.text("停用", "disabled", "無効").to_owned(),
            Err(error) => format!(
                "{}: {error}",
                language.text("配置无效", "invalid", "無効な設定")
            ),
        };
        println!(
            "{name:<20} {:<6} {:<8} {}\n{:>20} {}: {}",
            transport_label(config),
            state,
            config
                .url
                .clone()
                .or_else(|| config.command.clone())
                .unwrap_or_default(),
            "",
            language.text("鉴权", "auth", "認証"),
            auth_label(home, name, config, language)
        );
    }
    Ok(())
}

fn transport_label(config: &McpServerConfig) -> &'static str {
    match config.validated_transport("") {
        Ok(McpTransportKind::Stdio) => "stdio",
        Ok(McpTransportKind::StreamableHttp) => "http",
        Err(_) => "?",
    }
}

async fn login(
    home: &Path,
    servers: &BTreeMap<String, McpServerConfig>,
    name: &str,
    timeout: u64,
    no_browser: bool,
    language: Language,
) -> Result<()> {
    let config = server(servers, name)?;
    config.validate(name)?;
    let Some(url) = config.url.as_deref() else {
        bail!(
            "{}",
            language.text(
                "这是一个 stdio 服务，没有登录一说。",
                "this is a stdio server; there is nothing to log in to.",
                "stdio サーバーにはログインはありません。"
            )
        );
    };
    let Some(oauth_config) = &config.oauth else {
        bail!(
            "{}",
            language.text(
                "这个服务没有配置 oauth；静态 token 请用 bearer_token_env。",
                "this server has no oauth section; use bearer_token_env for a static token.",
                "このサーバーには oauth の設定がありません。静的トークンは bearer_token_env を使ってください。"
            )
        );
    };
    let client_secret = match &oauth_config.client_secret_env {
        Some(variable) => Some(std::env::var(variable).with_context(|| {
            format!("oauth.client_secret_env points at {variable}, which is not set")
        })?),
        None => None,
    };
    let progress = |line: &str| println!("{line}");
    let open = move |authorize_url: &str| {
        if !no_browser && !open_in_browser(authorize_url) {
            eprintln!(
                "{}",
                language.text(
                    "打不开浏览器，请手动打开上面的 URL。",
                    "could not open a browser; open the URL above by hand.",
                    "ブラウザを開けませんでした。上の URL を手動で開いてください。"
                )
            );
        }
    };
    let outcome = oauth::login(
        oauth::LoginRequest {
            home,
            server: name,
            url,
            config: oauth_config,
            client_secret,
            timeout: Duration::from_secs(timeout.max(10)),
        },
        &open,
        &progress,
    )
    .await?;
    println!(
        "{}: {} · client_id {}{} · scope {} · {}",
        language.text("已登录", "signed in", "ログイン済み"),
        outcome.authorization_server,
        outcome.client_id,
        if outcome.registered_dynamically {
            language.text("（动态注册）", " (registered dynamically)", "（動的登録）")
        } else {
            ""
        },
        outcome.scope.as_deref().unwrap_or("-"),
        match outcome.expires_at {
            Some(at) => format!(
                "{} {}",
                language.text("有效期至", "valid until", "有効期限"),
                willdeep_core::session::format_iso8601(at)
            ),
            None => language
                .text("无过期时间", "no expiry", "期限なし")
                .to_owned(),
        }
    );
    // 登完立刻连一次：token 拿到了但服务连不上，现在就该知道，而不是下一轮才发现。
    let mut only = BTreeMap::new();
    only.insert(
        name.to_owned(),
        McpServerConfig {
            enabled: true,
            ..config.clone()
        },
    );
    let registry = McpRegistry::connect_in(Some(home), &only).await?;
    if !registry.has_server(name) {
        bail!(
            "{}",
            language.text(
                "登录成功，但连接服务失败（见上方警告）。",
                "signed in, but connecting to the server failed (see the warning above).",
                "ログインは成功しましたが、サーバーへの接続に失敗しました（上の警告を参照）。"
            )
        );
    }
    println!(
        "{}: {}",
        language.text("可用工具", "tools available", "利用可能なツール"),
        registry.definitions().len()
    );
    Ok(())
}

fn logout(home: &Path, name: &str, language: Language) -> Result<()> {
    if oauth::forget_tokens(home, name)? {
        println!(
            "{}",
            language.text("已清除登录状态。", "signed out.", "ログアウトしました。")
        );
    } else {
        println!(
            "{}",
            language.text(
                "本来就没有登录状态。",
                "nothing to sign out of.",
                "ログイン状態はありませんでした。"
            )
        );
    }
    Ok(())
}

async fn tools(
    home: &Path,
    servers: &BTreeMap<String, McpServerConfig>,
    name: &str,
    json: bool,
    language: Language,
) -> Result<()> {
    let config = server(servers, name)?;
    let mut only = BTreeMap::new();
    only.insert(
        name.to_owned(),
        McpServerConfig {
            enabled: true,
            ..config.clone()
        },
    );
    let registry = McpRegistry::connect_in(Some(home), &only).await?;
    if !registry.has_server(name) {
        bail!(
            "{}",
            language.text(
                "连接失败（见上方警告）。",
                "connection failed (see the warning above).",
                "接続に失敗しました（上の警告を参照）。"
            )
        );
    }
    let definitions = registry.definitions();
    if json {
        println!("{}", serde_json::to_string_pretty(&definitions)?);
        return Ok(());
    }
    if definitions.is_empty() {
        println!(
            "{}",
            language.text(
                "连上了，但这个服务没有工具（可能只提供资源）。",
                "connected, but this server exposes no tools (it may only serve resources).",
                "接続しましたが、このサーバーにはツールがありません（リソースのみの可能性）。"
            )
        );
        return Ok(());
    }
    for definition in definitions {
        println!(
            "{:<48} {}",
            definition.name,
            definition.description.lines().next().unwrap_or_default()
        );
    }
    Ok(())
}

/// 尽力打开系统浏览器；打不开也不是错——URL 已经打印出来了。
fn open_in_browser(url: &str) -> bool {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        command.arg(url);
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "start", "", url]);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(url);
        command
    };
    command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}
