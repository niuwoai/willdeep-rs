//! Bundled short-drama studio. Installation never grants plugin permissions.
use std::path::Path;

use anyhow::{Context, Result};
use rust_embed::RustEmbed;
use willdeep_core::plugin::{PluginHost, installed_versions};

pub(crate) const ID: &str = "willdeep-video-studio";
pub(crate) const VERSION: &str = "0.46.0-rc1";

#[derive(RustEmbed)]
#[folder = "assets/willdeep-video-studio"]
struct VideoStudioAssets;

pub(crate) async fn ensure_installed(home: &Path) -> Result<()> {
    if installed_versions(&PluginHost::shared_root(home), ID)
        .iter()
        .any(|version| version == VERSION)
    {
        return Ok(());
    }
    let staging = std::env::temp_dir().join(format!("video-studio-{}", uuid::Uuid::new_v4()));
    let result = async {
        write_package(&staging)?;
        crate::plugin_cmd::install_one(home, &staging)?;
        Ok(())
    }
    .await;
    if let Err(error) = std::fs::remove_dir_all(&staging) {
        eprintln!("warning: video_studio_staging_cleanup_failed error={error}");
    }
    result
}

fn write_package(directory: &Path) -> Result<()> {
    for name in VideoStudioAssets::iter() {
        let path = directory.join(name.as_ref());
        std::fs::create_dir_all(path.parent().context("video studio asset has no parent")?)?;
        let asset = VideoStudioAssets::get(&name).context("missing bundled video studio asset")?;
        std::fs::write(path, asset.data)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bundled_studio_installs_without_approval_and_preserves_existing_package() {
        let home = std::env::temp_dir().join(format!("video-studio-test-{}", uuid::Uuid::new_v4()));
        ensure_installed(&home).await.unwrap();
        let host = PluginHost::discover(&home).unwrap();
        let package = host.package(ID).unwrap();
        assert_eq!(package.version, VERSION);
        assert!(package.root.join("ui/dist/index.html").is_file());
        assert!(package.root.join("server/video_studio.rb").is_file());
        assert!(!host.is_enabled(ID).await);
        assert!(host.approval_gap(ID).await.unwrap().is_some());
        let marker = package.root.join("preserved.txt");
        std::fs::write(&marker, "existing package").unwrap();
        ensure_installed(&home).await.unwrap();
        assert!(marker.is_file());
        std::fs::remove_dir_all(home).unwrap();
    }
}
