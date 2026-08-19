use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const MAX_SKILLS: usize = 300;
const MAX_RESOURCE_CHARS: usize = 48_000;

/// Context tier a skill declares for itself (`tier:` in SKILL.md frontmatter).
///
/// This is a *dispatch hint*, not a quality grade: it answers "how much
/// context does a run of this skill actually need, and can its result be
/// verified without trusting the model". The point is sovereignty as much as
/// cost — in an air-gapped deployment the only models available are usually
/// the ones a worker or standard tier can run on, and a skill library that
/// silently assumes a frontier model stops working the day it goes on-prem.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkillTier {
    /// 32–64K window, verifiable or strongly templated work. Deliverable to
    /// the small-context skill workers.
    Worker,
    /// ~256K window: the session's default tier. Ordinary development loops,
    /// reviews, single-module work.
    Standard,
    /// Long-context work: whole-repo reasoning, long-form writing, huge
    /// source material. Runs on the largest window available.
    Deep,
}

impl SkillTier {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "worker" | "small" => Some(Self::Worker),
            "standard" | "medium" => Some(Self::Standard),
            "deep" | "large" => Some(Self::Deep),
            // Unknown spellings are user data, not errors: a skill written
            // for some other tool must not break discovery here.
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Worker => "worker",
            Self::Standard => "standard",
            Self::Deep => "deep",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    pub identifier: String,
    pub name: String,
    pub description: String,
    /// Declared context tier, if the skill's frontmatter names one.
    pub tier: Option<SkillTier>,
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
                let (name, description, tier) = metadata(&body, &fallback);
                let identifier = normalize(&fallback);
                if !identifier.is_empty() {
                    by_id.entry(identifier.clone()).or_insert(Skill {
                        identifier,
                        name,
                        description,
                        tier,
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
            .map(|s| match s.tier {
                // The tier rides in the listing so the dispatch decision can
                // be made *before* the skill body is read: a worker-tier
                // skill is a spawn_agent candidate, not a reason to burn the
                // parent's window.
                Some(tier) => format!(
                    "- {} | {} | tier={} | {}",
                    s.identifier,
                    s.name,
                    tier.as_str(),
                    s.description
                ),
                None => format!("- {} | {} | {}", s.identifier, s.name, s.description),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Bounded catalog view for the always-on system prompt. Full skill
    /// bodies stay behind `read_skill`, and the complete index stays behind
    /// `list_skills`; this prevents dozens of unrelated descriptions from
    /// taxing every small-context turn.
    pub fn routing_summary(&self, max_chars: usize) -> String {
        if max_chars == 0 {
            return String::new();
        }
        let mut skills = self.skills.iter().collect::<Vec<_>>();
        skills.sort_by_key(|skill| match skill.tier {
            Some(SkillTier::Worker) => 0,
            Some(SkillTier::Standard) | None => 1,
            Some(SkillTier::Deep) => 2,
        });
        let mut output = String::new();
        let mut included = 0;
        for skill in skills {
            let line = match skill.tier {
                Some(tier) => format!(
                    "- {} | tier={} | {}\n",
                    skill.identifier,
                    tier.as_str(),
                    skill.description
                ),
                None => format!("- {} | {}\n", skill.identifier, skill.description),
            };
            if output.chars().count().saturating_add(line.chars().count()) > max_chars {
                break;
            }
            output.push_str(&line);
            included += 1;
        }
        if included < self.skills.len() {
            let suffix = format!(
                "[{} more skill(s); search with list_skills]\n",
                self.skills.len() - included
            );
            let remaining = max_chars.saturating_sub(output.chars().count());
            output.extend(suffix.chars().take(remaining));
        }
        output.trim_end().to_owned()
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

fn metadata(body: &str, fallback: &str) -> (String, String, Option<SkillTier>) {
    let mut name = None;
    let mut description = None;
    let mut tier = None;
    if body.starts_with("---") {
        for line in body.lines().skip(1).take_while(|line| *line != "---") {
            if let Some(value) = line.strip_prefix("name:") {
                name = Some(value.trim().trim_matches(['\'', '"']).to_owned());
            }
            if let Some(value) = line.strip_prefix("description:") {
                description = Some(value.trim().trim_matches(['\'', '"']).to_owned());
            }
            if let Some(value) = line.strip_prefix("tier:") {
                tier = SkillTier::parse(value.trim().trim_matches(['\'', '"']));
            }
        }
    }
    (
        name.filter(|v| !v.is_empty())
            .unwrap_or_else(|| fallback.to_owned()),
        description.unwrap_or_else(|| "Installed skill".to_owned()),
        tier,
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
    fn routing_summary_is_bounded_and_prioritizes_worker_skills() {
        let catalog = SkillCatalog {
            skills: vec![
                Skill {
                    identifier: "deep-book".to_owned(),
                    name: "Deep Book".to_owned(),
                    description: "d".repeat(200),
                    tier: Some(SkillTier::Deep),
                    path: PathBuf::new(),
                },
                Skill {
                    identifier: "worker-check".to_owned(),
                    name: "Worker Check".to_owned(),
                    description: "bounded verifier".to_owned(),
                    tier: Some(SkillTier::Worker),
                    path: PathBuf::new(),
                },
            ],
        };

        let summary = catalog.routing_summary(96);

        assert!(summary.chars().count() <= 96);
        assert!(summary.starts_with("- worker-check | tier=worker"));
        assert!(summary.contains("more skill"));
    }

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

    /// The tier is a dispatch hint read before the skill body: worker-tier
    /// skills go to spawn_agent, not into the parent's window. Both
    /// directions matter — a declared tier must surface, and a skill without
    /// one (or with a spelling from some other tool) must not break or lie.
    #[test]
    fn a_declared_tier_surfaces_and_an_unknown_one_stays_silent() {
        let root = std::env::temp_dir().join(format!("willdeep-tier-{}", uuid::Uuid::new_v4()));
        for (name, tier_line) in [
            ("convert", "tier: worker\n"),
            ("write", "tier: epic\n"),
            ("plain", ""),
        ] {
            let dir = root.join(".willdeep/skills").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: test\n{tier_line}---\n"),
            )
            .unwrap();
        }
        let catalog = SkillCatalog::discover(&root, &[]);
        let tier_of = |id: &str| {
            catalog
                .list()
                .iter()
                .find(|skill| skill.identifier == id)
                .expect("skill discovered")
                .tier
        };
        assert_eq!(tier_of("convert"), Some(SkillTier::Worker));
        assert_eq!(tier_of("write"), None, "unknown spellings are not errors");
        assert_eq!(tier_of("plain"), None);
        assert!(
            catalog
                .summary()
                .contains("convert | convert | tier=worker |")
        );
        assert!(!catalog.summary().contains("tier=epic"));
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
