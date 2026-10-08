//! Pinned first-party configuration plugin; installation never grants permissions.
use std::path::Path;

use anyhow::{Context, Result};
use rust_embed::RustEmbed;
use willdeep_core::plugin::PluginHost;

pub(crate) const ID: &str = "willdeep-config";

#[derive(RustEmbed)]
#[folder = "assets/willdeep-config"]
struct ConfigPluginAssets;

pub(crate) async fn ensure_installed(home: &Path) -> Result<()> {
    let host = PluginHost::discover(home)?;
    if host.packages().iter().any(|package| package.id == ID) {
        return Ok(());
    }
    let staging = std::env::temp_dir().join(format!("willdeep-config-{}", uuid::Uuid::new_v4()));
    let result = async {
        for name in ConfigPluginAssets::iter() {
            let path = staging.join(name.as_ref());
            std::fs::create_dir_all(path.parent().context("plugin asset has no parent")?)?;
            let asset = ConfigPluginAssets::get(&name).context("missing bundled config asset")?;
            std::fs::write(path, asset.data)?;
        }
        crate::plugin_cmd::install_package(home, &staging, false).await
    }
    .await;
    if let Err(error) = std::fs::remove_dir_all(&staging) {
        eprintln!("warning: config_plugin_staging_cleanup_failed error={error}");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fresh_install_requires_approval_and_repeated_install_preserves_it() {
        let home =
            std::env::temp_dir().join(format!("config-plugin-test-{}", uuid::Uuid::new_v4()));
        ensure_installed(&home).await.unwrap();
        let host = PluginHost::discover(&home).unwrap();
        assert!(host.package(ID).is_ok());
        assert!(!host.is_enabled(ID).await);
        assert!(host.approval_gap(ID).await.unwrap().is_some());
        host.approve(ID, 1).await.unwrap();
        ensure_installed(&home).await.unwrap();
        let host = PluginHost::discover(&home).unwrap();
        assert!(host.approval_gap(ID).await.unwrap().is_none());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn configuration_plugin_reads_the_hosts_custom_configuration() {
        let home =
            std::env::temp_dir().join(format!("config-plugin-path-{}", uuid::Uuid::new_v4()));
        ensure_installed(&home).await.unwrap();
        let path = home.join("selected.toml");
        std::fs::write(&path, "version = 1\n").unwrap();
        let host = PluginHost::discover(&home).unwrap();
        host.set_config_path(path.clone());
        host.approve(ID, 1).await.unwrap();
        host.set_enabled(ID, true).await.unwrap().unwrap();
        let result = host
            .execute_command(ID, "config.snapshot", serde_json::json!({}))
            .await
            .unwrap();
        let willdeep_core::plugin::CommandOutcome::Tool(value) = result else {
            panic!("expected configuration snapshot");
        };
        let text = value["content"][0]["text"].as_str().unwrap();
        let snapshot: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(snapshot["path"].as_str(), path.to_str());
        assert!(snapshot["exists"].as_bool().unwrap());
        drop(host);
        std::fs::remove_dir_all(home).unwrap();
    }
}
