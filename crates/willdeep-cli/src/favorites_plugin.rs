//! Pinned first-party favorites package; permissions remain user controlled.
use std::path::Path;

use anyhow::{Context, Result};
use rust_embed::RustEmbed;
use willdeep_core::plugin::{PluginHost, installed_versions};

pub(crate) const ID: &str = "willdeep-favorites";
pub(crate) const VERSION: &str = "2.3.0";

#[derive(RustEmbed)]
#[folder = "assets/willdeep-favorites"]
struct FavoritesAssets;

pub(crate) async fn ensure_installed(home: &Path) -> Result<()> {
    if installed_versions(&PluginHost::shared_root(home), ID)
        .iter()
        .any(|version| version == VERSION)
    {
        return Ok(());
    }
    install_package(home, false, true).await
}

pub(crate) async fn install(home: &Path, enable: bool) -> Result<()> {
    install_package(home, enable, false).await
}

async fn install_package(home: &Path, enable: bool, quiet: bool) -> Result<()> {
    let staging = std::env::temp_dir().join(format!("willdeep-favorites-{}", uuid::Uuid::new_v4()));
    let result = async {
        write_package(&staging)?;
        if quiet {
            // Startup must preserve the headless JSON/NDJSON stdout contract.
            crate::plugin_cmd::install_one(home, &staging)?;
            Ok(())
        } else {
            crate::plugin_cmd::install_package(home, &staging, enable).await
        }
    }
    .await;
    if let Err(error) = std::fs::remove_dir_all(&staging) {
        eprintln!("warning: favorites_staging_cleanup_failed error={error}");
    }
    result
}

fn write_package(directory: &Path) -> Result<()> {
    for name in FavoritesAssets::iter() {
        let path = directory.join(name.as_ref());
        std::fs::create_dir_all(path.parent().context("favorites asset has no parent")?)?;
        let asset = FavoritesAssets::get(&name).context("missing bundled favorites asset")?;
        std::fs::write(path, asset.data)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn upgraded_favorites_retains_old_version_and_requires_new_approval() {
        let home = std::env::temp_dir().join(format!("favorites-upgrade-{}", uuid::Uuid::new_v4()));
        let source = home.join("old-package");
        write_package(&source).unwrap();
        let manifest = source.join(".codex-plugin/plugin.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
        value["version"] = serde_json::json!("2.2.1");
        std::fs::write(manifest, serde_json::to_vec(&value).unwrap()).unwrap();
        crate::plugin_cmd::install_package(&home, &source, true)
            .await
            .unwrap();
        ensure_installed(&home).await.unwrap();
        let host = PluginHost::discover(&home).unwrap();
        assert_eq!(host.package(ID).unwrap().version, VERSION);
        assert!(host.approval_gap(ID).await.unwrap().is_some());
        assert!(
            home.join("plugins/willdeep-favorites/2.2.1/.codex-plugin/plugin.json")
                .is_file()
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn packaged_favorites_preserves_data_and_requires_approval() {
        let home = std::env::temp_dir().join(format!("favorites-package-{}", uuid::Uuid::new_v4()));
        let data = home.join("plugin-data/willdeep-favorites/favorites.json");
        std::fs::create_dir_all(data.parent().unwrap()).unwrap();
        std::fs::write(&data, "preserved user data").unwrap();
        ensure_installed(&home).await.unwrap();
        let host = PluginHost::discover(&home).unwrap();
        assert_eq!(host.package(ID).unwrap().version, VERSION);
        assert!(!host.is_enabled(ID).await);
        assert!(host.approval_gap(ID).await.unwrap().is_some());
        host.approve(ID, 1).await.unwrap();
        ensure_installed(&home).await.unwrap();
        assert!(
            PluginHost::discover(&home)
                .unwrap()
                .approval_gap(ID)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            std::fs::read_to_string(data).unwrap(),
            "preserved user data"
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn favorites_mcp_stores_rich_content_inside_selected_home() {
        let home = std::env::temp_dir().join(format!("favorites-mcp-{}", uuid::Uuid::new_v4()));
        install(&home, true).await.unwrap();
        let host = PluginHost::discover(&home).unwrap();
        let outcome = host.execute_command(ID, "favorites.add", serde_json::json!({
            "content": {"version": 1, "nodes": [{"type":"p","children":[{"type":"strong","children":[{"type":"text","text":"隔离富文本"}]}]}]}
        })).await.unwrap();
        let willdeep_core::plugin::CommandOutcome::Tool(value) = outcome else {
            panic!("expected MCP result")
        };
        let result: serde_json::Value =
            serde_json::from_str(value["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["item"]["text"], "隔离富文本");
        let stored: serde_json::Value = serde_json::from_slice(
            &std::fs::read(home.join("plugin-data/willdeep-favorites/favorites.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(stored["items"][0]["content"]["version"], 1);
        drop(host);
        std::fs::remove_dir_all(home).unwrap();
    }
}
