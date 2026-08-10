use std::path::PathBuf;
use std::process::Command;

use anyhow::{Result, bail};
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
}
