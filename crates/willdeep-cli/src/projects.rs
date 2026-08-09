use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: Uuid,
    pub display_name: String,
    pub folder_paths: Vec<PathBuf>,
}

pub fn load() -> Vec<Project> {
    project_file()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default()
}

pub fn resolve(name_or_id: &str) -> Result<PathBuf> {
    let project = load()
        .into_iter()
        .find(|project| {
            project.id.to_string() == name_or_id
                || project.display_name.eq_ignore_ascii_case(name_or_id)
        })
        .with_context(|| format!("project not found: {name_or_id}"))?;
    project
        .folder_paths
        .into_iter()
        .next()
        .context("project has no folders")
}

fn project_file() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Some(
            PathBuf::from(std::env::var_os("HOME")?)
                .join("Library/Application Support/WillDeep/projects.json"),
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}
