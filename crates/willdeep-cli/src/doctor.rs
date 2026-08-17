use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::config::{LoadedConfig, default_config_path};

#[derive(Clone, Debug)]
pub(crate) struct DoctorOptions {
    pub config_path: Option<PathBuf>,
    pub profile: Option<String>,
    pub workspace: Option<PathBuf>,
    pub home: PathBuf,
    pub api_base_present: bool,
    pub api_key_present: bool,
    pub model_present: bool,
    pub json: bool,
    pub bundle: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CheckStatus {
    Pass,
    Warning,
    Fail,
}

#[derive(Clone, Debug, Serialize)]
struct DoctorCheck {
    name: &'static str,
    status: CheckStatus,
    summary: String,
}

#[derive(Clone, Debug, Serialize)]
struct DoctorReport {
    version: &'static str,
    platform: String,
    overall: CheckStatus,
    checks: Vec<DoctorCheck>,
}

pub(crate) async fn run(options: DoctorOptions) -> Result<()> {
    let report = collect(&options).await;
    if let Some(path) = options.bundle.as_deref() {
        write_diagnostic_bundle(path, &report, &options)?;
    }
    if options.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("WillDeep Doctor {}", report.version);
        for check in &report.checks {
            println!(
                "{}\t{}\t{}",
                status_label(check.status),
                check.name,
                check.summary
            );
        }
        println!("overall\t{}", status_label(report.overall));
    }
    if report.overall == CheckStatus::Fail {
        bail!("doctor found one or more failed checks");
    }
    Ok(())
}

#[derive(Serialize)]
struct ConfigSummary {
    present: bool,
    valid: bool,
    schema_version: Option<u32>,
    provider_profiles: usize,
    subagent_profiles: usize,
    mcp_servers: usize,
    skill_roots: usize,
}

fn config_summary(options: &DoctorOptions) -> ConfigSummary {
    let path = options
        .config_path
        .clone()
        .map(Ok)
        .unwrap_or_else(default_config_path);
    let Ok(path) = path else {
        return ConfigSummary {
            present: false,
            valid: false,
            schema_version: None,
            provider_profiles: 0,
            subagent_profiles: 0,
            mcp_servers: 0,
            skill_roots: 0,
        };
    };
    if !path.exists() {
        return ConfigSummary {
            present: false,
            valid: false,
            schema_version: None,
            provider_profiles: 0,
            subagent_profiles: 0,
            mcp_servers: 0,
            skill_roots: 0,
        };
    }
    match LoadedConfig::load(Some(&path)) {
        Ok(loaded) => ConfigSummary {
            present: true,
            valid: true,
            schema_version: loaded.file.version,
            provider_profiles: loaded.file.providers.len(),
            subagent_profiles: loaded.file.subagents.len(),
            mcp_servers: loaded.file.mcp_servers.len(),
            skill_roots: loaded.file.skills.roots.len(),
        },
        Err(_) => ConfigSummary {
            present: true,
            valid: false,
            schema_version: None,
            provider_profiles: 0,
            subagent_profiles: 0,
            mcp_servers: 0,
            skill_roots: 0,
        },
    }
}

fn write_diagnostic_bundle(
    path: &Path,
    report: &DoctorReport,
    options: &DoctorOptions,
) -> Result<()> {
    if path.exists() {
        bail!("diagnostic bundle already exists: {}", path.display());
    }
    let report_json = serde_json::to_vec_pretty(report)?;
    let config_json = serde_json::to_vec_pretty(&config_summary(options))?;
    let readme = b"WillDeep diagnostic bundle\n\nThis archive intentionally excludes configuration values, API keys, Runtime tokens, Provider addresses, prompts, tool payloads, logs, and local paths.\n";
    let bytes = stored_zip(&[
        ("doctor.json", report_json.as_slice()),
        ("config-summary.json", config_json.as_slice()),
        ("README.txt", readme.as_slice()),
    ])?;
    write_private_atomic(path, &bytes)
}

fn stored_zip(entries: &[(&str, &[u8])]) -> Result<Vec<u8>> {
    if entries.len() > u16::MAX as usize {
        bail!("too many diagnostic bundle entries");
    }
    let mut output = Vec::new();
    let mut central = Vec::new();
    for (name, content) in entries {
        let name = name.as_bytes();
        let name_len = u16::try_from(name.len()).context("diagnostic filename is too long")?;
        let size = u32::try_from(content.len()).context("diagnostic entry is too large")?;
        let offset = u32::try_from(output.len()).context("diagnostic bundle is too large")?;
        let crc = crc32fast::hash(content);

        push_u32(&mut output, 0x0403_4b50);
        push_u16(&mut output, 20);
        push_u16(&mut output, 0);
        push_u16(&mut output, 0);
        push_u16(&mut output, 0);
        push_u16(&mut output, 0);
        push_u32(&mut output, crc);
        push_u32(&mut output, size);
        push_u32(&mut output, size);
        push_u16(&mut output, name_len);
        push_u16(&mut output, 0);
        output.extend_from_slice(name);
        output.extend_from_slice(content);

        push_u32(&mut central, 0x0201_4b50);
        push_u16(&mut central, 0x0314);
        push_u16(&mut central, 20);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u32(&mut central, crc);
        push_u32(&mut central, size);
        push_u32(&mut central, size);
        push_u16(&mut central, name_len);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u32(&mut central, 0o100600 << 16);
        push_u32(&mut central, offset);
        central.extend_from_slice(name);
    }
    let central_offset = u32::try_from(output.len()).context("diagnostic bundle is too large")?;
    let central_size = u32::try_from(central.len()).context("diagnostic bundle is too large")?;
    output.extend_from_slice(&central);
    push_u32(&mut output, 0x0605_4b50);
    push_u16(&mut output, 0);
    push_u16(&mut output, 0);
    push_u16(&mut output, entries.len() as u16);
    push_u16(&mut output, entries.len() as u16);
    push_u32(&mut output, central_size);
    push_u32(&mut output, central_offset);
    push_u16(&mut output, 0);
    Ok(output)
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !parent.is_dir() {
        bail!(
            "diagnostic bundle parent directory does not exist: {}",
            parent.display()
        );
    }
    let file_name = path
        .file_name()
        .context("diagnostic bundle path has no filename")?
        .to_string_lossy();
    let temporary = parent.join(format!(
        ".{file_name}.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut file = options.open(&temporary).with_context(|| {
            format!("create temporary diagnostic bundle in {}", parent.display())
        })?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::hard_link(&temporary, path).with_context(|| {
            format!(
                "publish diagnostic bundle without overwriting {}",
                path.display()
            )
        })?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&temporary);
    result
}

fn push_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

async fn collect(options: &DoctorOptions) -> DoctorReport {
    let mut checks = Vec::new();
    check_config(options, &mut checks);
    check_workspace(options, &mut checks);
    check_git(options, &mut checks);
    check_web_assets(&mut checks);
    check_runtime(options, &mut checks).await;
    let overall = if checks.iter().any(|check| check.status == CheckStatus::Fail) {
        CheckStatus::Fail
    } else if checks
        .iter()
        .any(|check| check.status == CheckStatus::Warning)
    {
        CheckStatus::Warning
    } else {
        CheckStatus::Pass
    };
    DoctorReport {
        version: env!("CARGO_PKG_VERSION"),
        platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        overall,
        checks,
    }
}

fn check_config(options: &DoctorOptions, checks: &mut Vec<DoctorCheck>) {
    let path = match options
        .config_path
        .clone()
        .map(Ok)
        .unwrap_or_else(default_config_path)
    {
        Ok(path) => path,
        Err(_) => {
            checks.push(check(
                "config",
                CheckStatus::Fail,
                "cannot resolve config path",
            ));
            return;
        }
    };
    if !path.exists() {
        checks.push(check(
            "config",
            CheckStatus::Warning,
            "not created; TUI onboarding or `willdeep config init` can create it",
        ));
        check_cli_provider(options, checks);
        return;
    }
    let loaded = match LoadedConfig::load(Some(&path)) {
        Ok(loaded) => loaded,
        Err(_) => {
            checks.push(check(
                "config",
                CheckStatus::Fail,
                "invalid or unsafe; run `willdeep config check` for details",
            ));
            return;
        }
    };
    checks.push(check(
        "config",
        CheckStatus::Pass,
        format!("valid; provider_profiles={}", loaded.file.providers.len()),
    ));
    check_notifications(&loaded, checks);
    match loaded.select_provider(options.profile.as_deref()) {
        Ok(Some(provider)) => {
            let credentials = options.api_key_present
                || provider
                    .api_key
                    .as_deref()
                    .is_some_and(|value| !value.is_empty())
                || provider
                    .api_key_env
                    .as_deref()
                    .is_some_and(env_value_present);
            let endpoint = options.api_base_present
                || provider
                    .api_base
                    .as_deref()
                    .is_some_and(|value| !value.is_empty());
            let model = options.model_present
                || provider
                    .model
                    .as_deref()
                    .is_some_and(|value| !value.is_empty());
            let complete = credentials && endpoint && model;
            checks.push(check(
                "provider",
                if complete {
                    CheckStatus::Pass
                } else {
                    CheckStatus::Warning
                },
                format!(
                    "selected profile; endpoint={}; model={}; credentials={}",
                    availability(endpoint),
                    availability(model),
                    availability(credentials)
                ),
            ));
        }
        Ok(None) => check_cli_provider(options, checks),
        Err(_) => checks.push(check(
            "provider",
            CheckStatus::Fail,
            "requested provider profile does not exist",
        )),
    }
}

/// Reports whether attention webhooks will actually fire. The URL itself is
/// never included — a webhook endpoint can carry a token in its path or query,
/// and this report gets written into shareable diagnostic bundles.
fn check_notifications(loaded: &LoadedConfig, checks: &mut Vec<DoctorCheck>) {
    let settings = &loaded.file.notifications;
    let notifier = crate::notify::Notifier::new(settings, Path::new("."));
    if !notifier.is_enabled() {
        checks.push(check(
            "notifications",
            CheckStatus::Pass,
            "webhook disabled; attention stays local",
        ));
        return;
    }
    checks.push(check(
        "notifications",
        CheckStatus::Pass,
        format!(
            "webhook enabled; task_completed={}; attention_required={}",
            availability(settings.webhook_on_task_completed.unwrap_or(true)),
            availability(settings.webhook_on_attention_required.unwrap_or(true)),
        ),
    ));
}

fn check_cli_provider(options: &DoctorOptions, checks: &mut Vec<DoctorCheck>) {
    let complete = options.api_base_present && options.api_key_present && options.model_present;
    checks.push(check(
        "provider",
        if complete {
            CheckStatus::Pass
        } else {
            CheckStatus::Warning
        },
        if complete {
            "CLI Provider overrides are complete"
        } else {
            "Provider is incomplete; configure API base, model, and credentials"
        },
    ));
}

fn check_workspace(options: &DoctorOptions, checks: &mut Vec<DoctorCheck>) {
    let requested = options
        .workspace
        .clone()
        .or_else(|| std::env::current_dir().ok());
    let Some(requested) = requested else {
        checks.push(check(
            "workspace",
            CheckStatus::Fail,
            "cannot resolve current directory",
        ));
        return;
    };
    match requested.canonicalize() {
        Ok(path) if path.is_dir() => checks.push(check(
            "workspace",
            CheckStatus::Pass,
            "available and canonicalized",
        )),
        _ => checks.push(check(
            "workspace",
            CheckStatus::Fail,
            "path does not exist or is not a directory",
        )),
    }
}

fn check_git(options: &DoctorOptions, checks: &mut Vec<DoctorCheck>) {
    let version = Command::new("git").arg("--version").output();
    let Ok(version) = version else {
        checks.push(check(
            "git",
            CheckStatus::Fail,
            "Git executable is unavailable",
        ));
        return;
    };
    if !version.status.success() {
        checks.push(check("git", CheckStatus::Fail, "Git executable failed"));
        return;
    }
    let workspace = options
        .workspace
        .clone()
        .or_else(|| std::env::current_dir().ok());
    let repository = workspace.is_some_and(|workspace| {
        Command::new("git")
            .args(["rev-parse", "--is-inside-work-tree"])
            .current_dir(workspace)
            .output()
            .is_ok_and(|output| output.status.success())
    });
    checks.push(check(
        "git",
        if repository {
            CheckStatus::Pass
        } else {
            CheckStatus::Warning
        },
        if repository {
            "available; workspace is a Git worktree"
        } else {
            "available; workspace is not a Git worktree"
        },
    ));
}

fn check_web_assets(checks: &mut Vec<DoctorCheck>) {
    let available = crate::web::embedded_assets_available();
    checks.push(check(
        "web_assets",
        if available {
            CheckStatus::Pass
        } else {
            CheckStatus::Fail
        },
        if available {
            "embedded Web App is available"
        } else {
            "embedded Web App assets are missing"
        },
    ));
}

async fn check_runtime(options: &DoctorOptions, checks: &mut Vec<DoctorCheck>) {
    let runtime = crate::daemon::diagnostic(&options.home).await;
    checks.push(runtime_check(runtime));
}

fn runtime_check(runtime: crate::daemon::RuntimeDiagnostic) -> DoctorCheck {
    let (status, summary) = match runtime.status {
        "running" => {
            let version = runtime.version.as_deref().unwrap_or("unknown");
            let matches_client = version == env!("CARGO_PKG_VERSION");
            (
                if matches_client {
                    CheckStatus::Pass
                } else {
                    CheckStatus::Warning
                },
                format!(
                    "running; version={version}; version_match={}; transport={}; uptime={}s",
                    matches_client,
                    runtime.transport.unwrap_or("unknown"),
                    runtime.uptime_seconds.unwrap_or_default()
                ),
            )
        }
        "stale" => (
            CheckStatus::Fail,
            "state exists but Runtime is unreachable".to_owned(),
        ),
        _ => (
            CheckStatus::Warning,
            "stopped; it will start when Runtime-backed commands need it".to_owned(),
        ),
    };
    check("runtime", status, summary)
}

fn env_value_present(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

fn availability(value: bool) -> &'static str {
    if value { "available" } else { "missing" }
}

fn status_label(status: CheckStatus) -> &'static str {
    match status {
        CheckStatus::Pass => "PASS",
        CheckStatus::Warning => "WARN",
        CheckStatus::Fail => "FAIL",
    }
}

fn check(name: &'static str, status: CheckStatus, summary: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        name,
        status,
        summary: summary.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn report_is_machine_readable_and_never_serializes_inline_secrets() {
        let root = std::env::temp_dir().join(format!("willdeep-doctor-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("root");
        let config = root.join("config.toml");
        std::fs::write(
            &config,
            "version = 1\ndefault_provider = \"test\"\n[providers.test]\napi_base = \"https://example.invalid/v1\"\napi_key = \"doctor-secret\"\nmodel = \"test-model\"\n",
        )
        .expect("config");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600))
                .expect("permissions");
        }
        let report = collect(&DoctorOptions {
            config_path: Some(config),
            profile: None,
            workspace: Some(root.clone()),
            home: root.clone(),
            api_base_present: false,
            api_key_present: false,
            model_present: false,
            json: true,
            bundle: None,
        })
        .await;
        let json = serde_json::to_string(&report).expect("JSON");
        assert!(!json.contains("doctor-secret"));
        assert!(!json.contains("api_key"));
        assert!(!json.contains(&root.display().to_string()));
        assert!(json.contains("provider_profiles=1"));
        assert!(json.contains("web_assets"));
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn runtime_version_mismatch_is_visible_without_becoming_a_false_failure() {
        let result = runtime_check(crate::daemon::RuntimeDiagnostic {
            status: "running",
            version: Some("0.0.0-old".to_owned()),
            uptime_seconds: Some(10),
            transport: Some("unix_socket"),
        });
        assert_eq!(result.status, CheckStatus::Warning);
        assert!(result.summary.contains("version_match=false"));
        assert!(!result.summary.contains("token"));
    }

    #[tokio::test]
    async fn bundle_is_a_private_non_overwriting_zip_without_sensitive_values() {
        let root = std::env::temp_dir().join(format!("willdeep-bundle-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("root");
        let config = root.join("config.toml");
        std::fs::write(
            &config,
            "version = 1\n[providers.private-name]\napi_base = \"https://private.example/v1\"\napi_key = \"bundle-secret\"\nmodel = \"private-model\"\n",
        )
        .expect("config");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600))
                .expect("permissions");
        }
        let options = DoctorOptions {
            config_path: Some(config),
            profile: None,
            workspace: Some(root.clone()),
            home: root.clone(),
            api_base_present: false,
            api_key_present: false,
            model_present: false,
            json: false,
            bundle: None,
        };
        let report = collect(&options).await;
        let bundle = root.join("diagnostics.zip");
        write_diagnostic_bundle(&bundle, &report, &options).expect("bundle");
        let bytes = std::fs::read(&bundle).expect("read bundle");
        let entries = read_stored_zip_entries(&bytes);
        assert_eq!(
            entries
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["doctor.json", "config-summary.json", "README.txt"]
        );
        let all_content = entries
            .iter()
            .flat_map(|(_, content)| content.iter().copied())
            .collect::<Vec<_>>();
        let content = String::from_utf8(all_content).expect("UTF-8 bundle entries");
        for sensitive in [
            "bundle-secret",
            "private.example",
            "private-model",
            "private-name",
            root.to_string_lossy().as_ref(),
        ] {
            assert!(!content.contains(sensitive), "bundle leaked {sensitive}");
        }
        assert!(write_diagnostic_bundle(&bundle, &report, &options).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&bundle).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    fn read_stored_zip_entries(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut entries = Vec::new();
        let mut offset = 0usize;
        while bytes.get(offset..offset + 4) == Some(&0x0403_4b50u32.to_le_bytes()) {
            let crc = u32::from_le_bytes(bytes[offset + 14..offset + 18].try_into().unwrap());
            let size =
                u32::from_le_bytes(bytes[offset + 18..offset + 22].try_into().unwrap()) as usize;
            let name_len =
                u16::from_le_bytes(bytes[offset + 26..offset + 28].try_into().unwrap()) as usize;
            let extra_len =
                u16::from_le_bytes(bytes[offset + 28..offset + 30].try_into().unwrap()) as usize;
            let name_start = offset + 30;
            let content_start = name_start + name_len + extra_len;
            let content_end = content_start + size;
            let name =
                String::from_utf8(bytes[name_start..name_start + name_len].to_vec()).unwrap();
            let content = bytes[content_start..content_end].to_vec();
            assert_eq!(crc32fast::hash(&content), crc);
            entries.push((name, content));
            offset = content_end;
        }
        assert_eq!(
            bytes.get(offset..offset + 4),
            Some(0x0201_4b50u32.to_le_bytes().as_slice())
        );
        assert_eq!(
            bytes.get(bytes.len() - 22..bytes.len() - 18),
            Some(0x0605_4b50u32.to_le_bytes().as_slice())
        );
        entries
    }
}
