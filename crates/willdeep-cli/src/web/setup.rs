use super::*;

#[derive(Serialize)]
pub(super) struct SetupStatus {
    ready: bool,
    ruby_available: bool,
    config_path: String,
}

pub(super) async fn setup_status(State(state): State<Arc<WebState>>) -> Json<SetupStatus> {
    let path = state.config_path.clone();
    let profile = state.profile.clone();
    let ready = setup_ready(&path, profile.as_deref());
    let ruby_available = tokio::task::spawn_blocking(|| {
        std::process::Command::new("/usr/bin/ruby")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    })
    .await
    .unwrap_or(false);
    Json(SetupStatus {
        ready,
        ruby_available,
        config_path: path.display().to_string(),
    })
}

pub(super) fn setup_ready(path: &std::path::Path, profile: Option<&str>) -> bool {
    use clap::Parser;
    let Ok(loaded) = crate::config::LoadedConfig::load(Some(path)) else {
        return false;
    };
    let mut cli = crate::Cli::parse_from(["willdeep"]);
    cli.profile = profile.map(str::to_owned);
    let Ok(provider) = crate::harness::resolve_parent_provider_config(&cli, &loaded, None) else {
        return false;
    };
    reqwest::Url::parse(&provider.base_url)
        .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
        && !provider.model.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_requires_saved_default_provider_and_respects_requested_profile() {
        let home = std::env::temp_dir().join(format!("setup-ready-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let path = home.join("custom.toml");
        assert!(!setup_ready(&path, None));
        std::fs::write(&path, "version = 1\ndefault_provider = 'missing'\n").unwrap();
        assert!(!setup_ready(&path, None));
        std::fs::write(&path, "version = 1\ndefault_provider = 'configured'\n[providers.configured]\nprovider = 'openai-compatible'\napi_base = 'https://example.invalid/v1'\napi_key = 'test-placeholder'\nmodel = 'test-model'\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert!(setup_ready(&path, None));
        assert!(!setup_ready(&path, Some("not-configured")));
        std::fs::remove_dir_all(home).unwrap();
    }
}
