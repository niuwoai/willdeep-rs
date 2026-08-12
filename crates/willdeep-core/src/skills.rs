use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const MAX_SKILLS: usize = 300;
const MAX_RESOURCE_CHARS: usize = 48_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    pub identifier: String,
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Default)]
pub struct SkillCatalog {
    skills: Vec<Skill>,
}

impl SkillCatalog {
    pub fn discover(workspace: &Path, extra_roots: &[PathBuf]) -> Self {
        let mut roots = vec![
            workspace.join(".willdeep/skills"),
            workspace.join(".agents/skills"),
            workspace.join(".codex/skills"),
        ];
        if let Some(home) = home_dir() {
            roots.extend([
                home.join(".willdeep/skills"),
                home.join(".agents/skills"),
                home.join(".codex/skills"),
            ]);
        }
        roots.extend_from_slice(extra_roots);
        let mut by_id = BTreeMap::new();
        for root in roots {
            let Ok(entries) = std::fs::read_dir(root) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path().join("SKILL.md");
                let Ok(body) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let fallback = entry.file_name().to_string_lossy().into_owned();
                let (name, description) = metadata(&body, &fallback);
                let identifier = normalize(&fallback);
                if !identifier.is_empty() {
                    by_id.entry(identifier.clone()).or_insert(Skill {
                        identifier,
                        name,
                        description,
                        path,
                    });
                }
                if by_id.len() >= MAX_SKILLS {
                    break;
                }
            }
            if by_id.len() >= MAX_SKILLS {
                break;
            }
        }
        Self {
            skills: by_id.into_values().collect(),
        }
    }

    pub fn list(&self) -> &[Skill] {
        &self.skills
    }

    pub fn allow_only(mut self, allowed: &[String]) -> Self {
        if allowed.is_empty() {
            return self;
        }
        self.skills.retain(|skill| {
            allowed
                .iter()
                .any(|value| value == &skill.identifier || value == &skill.name)
        });
        self
    }

    pub fn summary(&self) -> String {
        self.skills
            .iter()
            .map(|s| format!("- {} | {} | {}", s.identifier, s.name, s.description))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn read(&self, name: &str, resource: Option<&str>) -> Result<String, SkillError> {
        let skill = self
            .skills
            .iter()
            .find(|s| s.identifier == name || s.name == name)
            .ok_or_else(|| SkillError::NotFound(name.to_owned()))?;
        let root = skill
            .path
            .parent()
            .expect("skill path has parent")
            .canonicalize()?;
        let path = match resource.filter(|v| !v.trim().is_empty()) {
            Some(value) => root.join(value).canonicalize()?,
            None => skill.path.canonicalize()?,
        };
        if !path.starts_with(&root) || !path.is_file() {
            return Err(SkillError::UnsafeResource(
                resource.unwrap_or_default().to_owned(),
            ));
        }
        let body = std::fs::read_to_string(&path)?;
        let limited = body.chars().take(MAX_RESOURCE_CHARS).collect::<String>();
        let suffix = if body.chars().count() > MAX_RESOURCE_CHARS {
            "\n\n[truncated]"
        } else {
            ""
        };
        Ok(format!(
            "Skill: {}\nResource: {}\nPath: {}\n\n{}{}",
            skill.identifier,
            path.strip_prefix(&root).unwrap_or(&path).display(),
            path.display(),
            limited.trim(),
            suffix
        ))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("skill not found: {0}")]
    NotFound(String),
    #[error("unsafe or missing skill resource: {0}")]
    UnsafeResource(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

fn metadata(body: &str, fallback: &str) -> (String, String) {
    let mut name = None;
    let mut description = None;
    if body.starts_with("---") {
        for line in body.lines().skip(1).take_while(|line| *line != "---") {
            if let Some(value) = line.strip_prefix("name:") {
                name = Some(value.trim().trim_matches(['\'', '"']).to_owned());
            }
            if let Some(value) = line.strip_prefix("description:") {
                description = Some(value.trim().trim_matches(['\'', '"']).to_owned());
            }
        }
    }
    (
        name.filter(|v| !v.is_empty())
            .unwrap_or_else(|| fallback.to_owned()),
        description.unwrap_or_else(|| "Installed skill".to_owned()),
    )
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_owned()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovers_and_reads_skill() {
        let root = std::env::temp_dir().join(format!("willdeep-skill-{}", uuid::Uuid::new_v4()));
        let dir = root.join(".willdeep/skills/reviewer");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            "---\nname: Reviewer\ndescription: Review code\n---\n# Steps",
        )
        .unwrap();
        let catalog = SkillCatalog::discover(&root, &[]);
        assert!(
            catalog
                .list()
                .iter()
                .any(|skill| skill.identifier == "reviewer")
        );
        assert!(catalog.read("reviewer", None).unwrap().contains("# Steps"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn workspace_allowlist_filters_discovered_skills() {
        let root = std::env::temp_dir().join(format!("willdeep-skills-{}", uuid::Uuid::new_v4()));
        for name in ["reader", "editor"] {
            let directory = root.join(".willdeep/skills").join(name);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                directory.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: test\n---\n"),
            )
            .unwrap();
        }
        let catalog = SkillCatalog::discover(&root, &[]).allow_only(&["reader".to_owned()]);
        assert_eq!(catalog.list().len(), 1);
        assert_eq!(catalog.list()[0].name, "reader");
        std::fs::remove_dir_all(root).unwrap();
    }
}
