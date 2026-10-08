use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use uuid::Uuid;

use crate::config;

const SOME_IM_ORIGIN: &str = "https://some.im";
const CLIENT_ID: &str = "willdeep";

/// Returns true when the user chose the browser configuration flow.
pub async fn run(explicit_path: Option<&Path>) -> Result<bool> {
    if !io::stdin().is_terminal() {
        bail!("first-use setup needs an interactive terminal; pass --config or provider flags");
    }
    println!("WillDeep 首次设置");
    println!(
        "1) 打开 WebApp，在 willdeep-config 中配置 Provider（推荐）\n2) some.im 浏览器登录\n3) 在终端填写 API Base / API Key"
    );
    let choice = prompt("选择 [1]: ")?;
    if choice.trim().is_empty() || choice.trim() == "1" {
        println!("将在本机启动 WebApp，终端会给出配置地址；保存配置后自动继续启动命令行。");
        return Ok(true);
    }
    let (provider, base, key, model) = if choice.trim() == "2" {
        some_im_login().await?
    } else if choice.trim() == "3" {
        let base = required("API Base: ")?;
        let key = required("API Key（输入会显示，请留意终端历史）: ")?;
        let model = required("模型名: ")?;
        ("openai-compatible".to_owned(), base, key, model)
    } else {
        bail!("请选择 1、2 或 3");
    };
    let path = explicit_path
        .map(Path::to_path_buf)
        .unwrap_or(config::default_config_path()?);
    write_config(&path, &provider, &base, &key, &model)?;
    println!("配置已保存到 {}（权限仅当前用户可读写）", path.display());
    Ok(false)
}

pub(crate) fn needs_web_setup(cli: &crate::Cli) -> bool {
    let path = cli
        .config
        .clone()
        .or_else(|| config::default_config_path().ok());
    !crate::zero_config_ready(cli)
        && path.is_some_and(|path| !crate::web::setup_ready(&path, cli.profile.as_deref()))
}

/// Keep the local configuration WebApp alive only for this CLI process.
pub(crate) struct SetupServer(tokio::task::JoinHandle<Result<()>>);

impl Drop for SetupServer {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(crate) async fn configure_in_browser(cli: &crate::Cli) -> Result<SetupServer> {
    if !io::stdin().is_terminal() {
        bail!("first-use setup needs an interactive terminal; pass --config or provider flags");
    }
    if !cli.listen.ip().is_loopback() {
        bail!("Provider setup requires a loopback listen address");
    }
    let config_path =
        std::path::absolute(cli.config.clone().unwrap_or(config::default_config_path()?))?;
    let workspace = cli
        .workspace
        .clone()
        .unwrap_or(std::env::current_dir()?)
        .canonicalize()?;
    // A dedicated ephemeral port avoids taking over an existing user's WebApp.
    let mut listen = cli.listen;
    listen.set_port(0);
    let (started, ready) = tokio::sync::oneshot::channel();
    let server = SetupServer(tokio::spawn(crate::web::serve(crate::web::WebConfig {
        listen,
        config_path: config_path.clone(),
        profile: cli.profile.clone(),
        workspaces: vec![workspace],
        home: crate::willdeep_home()?,
        language: crate::i18n::Language::parse(cli.language.as_deref())?,
        open_setup: true,
        setup_started: Some(started),
    })));
    let address = tokio::time::timeout(Duration::from_secs(30), ready)
        .await
        .context("等待配置 WebApp 启动超时")?
        .context("配置 WebApp 启动失败，请使用 --web 查看错误")?;
    println!(
        "尚未配置 Provider。willdeep-config 已随 CLI 内置。\n请打开 WebApp 配置地址：http://{address}/?setup=1#plugins\n配置插件地址：http://{address}/?setup=1#plugin/willdeep-config%3Aconfig\n首次使用请先在插件中心批准并启用 willdeep-config，然后添加 Provider、模型并设置默认 Provider。\n配置文件：{}\n终端正在等待保存配置，完成后自动继续；按 Ctrl+C 取消。",
        config_path.display()
    );
    println!(
        "插件需要 /usr/bin/ruby；缺少 Ruby 或无法使用浏览器时，可取消后运行 willdeep --onboarding，选择 3 在终端配置。"
    );
    loop {
        if crate::web::setup_ready(&config_path, cli.profile.as_deref()) {
            println!("Provider 配置已就绪，继续启动命令行。");
            return Ok(server);
        }
        if server.0.is_finished() {
            bail!("配置 WebApp 已停止，请重新运行 willdeep");
        }
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                result?;
                bail!("已取消首次配置");
            }
            _ = tokio::time::sleep(Duration::from_millis(500)) => {}
        }
    }
}

pub(crate) fn open_browser(url: &str) -> bool {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).status();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("rundll32.exe")
        .args(["url.dll,FileProtocolHandler", url])
        .status();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let result = std::process::Command::new("xdg-open").arg(url).status();
    result.is_ok_and(|status| status.success())
}

async fn some_im_login() -> Result<(String, String, String, String)> {
    let secret = std::env::var("WILLDEEP_CLIENT_LOGIN_SECRET").unwrap_or_default();
    if secret.trim().is_empty() {
        bail!(
            "浏览器登录需要 WILLDEEP_CLIENT_LOGIN_SECRET；它是客户端凭据，不能写进仓库。也可重跑 --onboarding 选择手动 API Key"
        )
    }
    let code = format!(
        "WD-{}-{}",
        &Uuid::new_v4().simple().to_string()[..4].to_uppercase(),
        &Uuid::new_v4().simple().to_string()[..4].to_uppercase()
    );
    let pair_token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let mut url = reqwest::Url::parse(&format!("{SOME_IM_ORIGIN}/customer/login"))?;
    let next = format!(
        "/customer?willdeep_return=1&code={code}&device_code={code}&willdeep_device_code={code}&pair_token={pair_token}&willdeep_pair_token={pair_token}"
    );
    url.query_pairs_mut()
        .append_pair("client_id", CLIENT_ID)
        .append_pair("code", &code)
        .append_pair("device_code", &code)
        .append_pair("willdeep_device_code", &code)
        .append_pair("pair_token", &pair_token)
        .append_pair("willdeep_pair_token", &pair_token)
        .append_pair("return_to", "willdeep")
        .append_pair("next", &next);
    println!("请在浏览器打开并登录：\n{url}\n\n设备码：{code}\nWillDeep 正在等待授权……");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    for _ in 0..180 {
        let mut status = reqwest::Url::parse(&format!(
            "{SOME_IM_ORIGIN}/api/v1/public/client-login/browser-status"
        ))?;
        status
            .query_pairs_mut()
            .append_pair("client_id", CLIENT_ID)
            .append_pair("code", &code)
            .append_pair("device_code", &code)
            .append_pair("willdeep_device_code", &code);
        let response = client
            .get(status)
            .bearer_auth(&pair_token)
            .header("X-WillDeep-Client-Secret", secret.trim())
            .send()
            .await?;
        if response.status().is_success() {
            let body: Value = response.json().await?;
            let data = body.get("data").unwrap_or(&body);
            let state = string(data, &["status", "state", "login_status"])
                .unwrap_or_default()
                .to_ascii_lowercase();
            if matches!(
                state.as_str(),
                "connected" | "success" | "completed" | "authenticated"
            ) {
                let key = string(
                    data,
                    &[
                        "api_key",
                        "apiKey",
                        "app_api_key",
                        "willdeep_api_key",
                        "api_token",
                    ],
                )
                .context("some.im 已确认登录，但未返回 API Key")?;
                // Browser login establishes credentials, not the routing
                // policy. Start new installations on the private-deployable
                // standard tier; `deep` remains an explicit escalation
                // profile instead of silently becoming every session's root.
                let model = string(data, &["standard_model", "standardModel"])
                    .unwrap_or_else(|| crate::model_defaults::SOMEIM_DEFAULT_MODEL.to_owned());
                return Ok((
                    "some-im".to_owned(),
                    "https://some.im/v1".to_owned(),
                    key,
                    model,
                ));
            }
            if matches!(state.as_str(), "expired" | "cancelled" | "timeout") {
                bail!("浏览器登录已过期，请重新运行 --onboarding");
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    bail!("等待浏览器登录超时，请重新运行 --onboarding")
}

fn string(value: &Value, names: &[&str]) -> Option<String> {
    for name in names {
        if let Some(value) = value
            .get(name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            return Some(value.to_owned());
        }
    }
    for wrapper in ["credentials", "credential"] {
        if let Some(nested) = value.get(wrapper)
            && let Some(value) = string(nested, names)
        {
            return Some(value);
        }
    }
    None
}

fn write_config(path: &Path, provider: &str, base: &str, key: &str, model: &str) -> Result<()> {
    let mut root = toml::map::Map::new();
    root.insert("version".to_owned(), toml::Value::Integer(1));
    root.insert(
        "default_provider".to_owned(),
        toml::Value::String("default".to_owned()),
    );
    let mut profile = toml::map::Map::new();
    profile.insert(
        "provider".to_owned(),
        toml::Value::String(provider.to_owned()),
    );
    profile.insert(
        "api".to_owned(),
        toml::Value::String("chat-completions".to_owned()),
    );
    profile.insert("api_base".to_owned(), toml::Value::String(base.to_owned()));
    profile.insert("api_key".to_owned(), toml::Value::String(key.to_owned()));
    profile.insert("model".to_owned(), toml::Value::String(model.to_owned()));
    let mut providers = toml::map::Map::new();
    providers.insert("default".to_owned(), toml::Value::Table(profile));
    root.insert("providers".to_owned(), toml::Value::Table(providers));
    std::fs::create_dir_all(path.parent().context("configuration path has no parent")?)?;
    std::fs::write(path, toml::to_string_pretty(&toml::Value::Table(root))?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn required(label: &str) -> Result<String> {
    let value = prompt(label)?;
    if value.trim().is_empty() {
        bail!("该项不能为空")
    }
    Ok(value.trim().to_owned())
}
fn prompt(label: &str) -> Result<String> {
    print!("{label}");
    io::stdout().flush()?;
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_existing_configuration_still_requires_setup() {
        use clap::Parser;
        let root = std::env::temp_dir().join(format!("onboarding-detection-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("config.toml");
        let mut cli = crate::Cli::parse_from(["willdeep"]);
        cli.config = Some(path.clone());
        if !crate::zero_config_ready(&cli) {
            assert!(needs_web_setup(&cli));
            std::fs::write(&path, "version = 1\n").unwrap();
            assert!(needs_web_setup(&cli));
            write_config(
                &path,
                "openai-compatible",
                "https://example.invalid/v1",
                "placeholder",
                "model",
            )
            .unwrap();
            assert!(!needs_web_setup(&cli));
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn extracts_credentials_from_supported_wrappers() {
        let value = serde_json::json!({"credentials": {"apiKey": "secret"}});
        assert_eq!(
            string(&value, &["api_key", "apiKey"]).as_deref(),
            Some("secret")
        );
    }

    #[test]
    fn writes_valid_private_config() {
        let root = std::env::temp_dir().join(format!("willdeep-onboarding-{}", Uuid::new_v4()));
        let path = root.join("config.toml");
        write_config(
            &path,
            "some-im",
            "https://some.im/v1",
            "placeholder",
            "model",
        )
        .unwrap();
        let parsed: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["default_provider"].as_str(), Some("default"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
