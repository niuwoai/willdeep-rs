//! `willdeep prompt`：查看提示词分段、校验候选变体、从改进候选起草变体
//! （`docs/PROMPT_RSI_OPERATIONS.md`）。只读代码里的提示词，不改任何东西。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use willdeep_core::prompt_sections::{
    PromptRole, PromptVariant, VARIANT_SCHEMA, base_section, check_variant, load_variant,
};

#[derive(Clone, Debug, Subcommand)]
pub(crate) enum PromptAction {
    /// List every role's sections with their size and the role's prompt bundle id.
    Sections {
        /// Only this role: main, input_suggestion or worker:<profile>.
        #[arg(long, value_name = "ROLE")]
        role: Option<String>,
    },
    /// Print the current text of one section (the starting point for a variant).
    Show { role: String, section: String },
    /// Run the structure and safety gate on a variant file and show what it changes.
    ///
    /// Exits non-zero when the variant is invalid; the same gate refuses to start
    /// `willdeep run --local` with an invalid WILLDEEP_PROMPT_VARIANT.
    Check { variant: PathBuf },
    /// Draft a variant from one improvement candidate of `willdeep feedback report --candidates`.
    ///
    /// The draft's text is the current section, to be rewritten by hand; the
    /// draft does not pass `prompt check` until it changes something.
    Draft {
        /// The candidates JSON file.
        #[arg(long, value_name = "PATH")]
        candidates: PathBuf,
        /// Which candidate (0-based).
        #[arg(long, default_value_t = 0)]
        index: usize,
        /// Write the draft here instead of stdout.
        #[arg(short, long, value_name = "PATH")]
        output: Option<PathBuf>,
    },
}

pub(crate) fn run(action: PromptAction) -> Result<()> {
    match action {
        PromptAction::Sections { role } => {
            let roles = match role {
                Some(role) => vec![parse_role(&role)?],
                None => PromptRole::all(),
            };
            print!("{}", render_sections(&roles));
            Ok(())
        }
        PromptAction::Show { role, section } => {
            let role = parse_role(&role)?;
            let text = base_section(&role, &section).with_context(|| {
                format!(
                    "{} has no section {section}; sections: {}",
                    role.name(),
                    role.sections().join(", ")
                )
            })?;
            println!("{text}");
            Ok(())
        }
        PromptAction::Check { variant } => {
            let (report, valid) = check_report(&variant)?;
            print!("{report}");
            if !valid {
                bail!("prompt variant {} is invalid", variant.display());
            }
            Ok(())
        }
        PromptAction::Draft {
            candidates,
            index,
            output,
        } => {
            let draft = draft_from_candidates(&candidates, index)?;
            let text = serde_json::to_string_pretty(&draft)?;
            match output {
                Some(path) => {
                    std::fs::write(&path, format!("{text}\n"))
                        .with_context(|| format!("write {}", path.display()))?;
                    eprintln!(
                        "Draft written to {}. Rewrite `text`, then run `willdeep prompt check {}`.",
                        path.display(),
                        path.display()
                    );
                }
                None => println!("{text}"),
            }
            Ok(())
        }
    }
}

fn parse_role(role: &str) -> Result<PromptRole> {
    PromptRole::parse(role).with_context(|| {
        format!("unknown role {role:?}; use main, input_suggestion or worker:<built-in profile>")
    })
}

fn role_bundle(role: &PromptRole) -> String {
    match role {
        PromptRole::Main => willdeep_core::prompt_bundle::main_bundle_with(None),
        PromptRole::InputSuggestion => {
            willdeep_core::prompt_bundle::input_suggestion_bundle_with(None)
        }
        PromptRole::Worker(id) => willdeep_core::prompt_bundle::builtin_profile_list()
            .iter()
            .find(|profile| &profile.id == id)
            .map(|profile| willdeep_core::prompt_bundle::worker_bundle_with(profile, true, None))
            .unwrap_or_default(),
    }
}

fn render_sections(roles: &[PromptRole]) -> String {
    let mut out = String::new();
    for role in roles {
        out.push_str(&format!("{}  {}\n", role.name(), role_bundle(role)));
        for section in role.sections() {
            match base_section(role, section) {
                Some(text) => out.push_str(&format!(
                    "  {section:<20} {:>6} chars  {}\n",
                    text.chars().count(),
                    willdeep_core::feedback::text_hash(&text)
                )),
                None => out.push_str(&format!("  {section:<20} (not sent for this profile)\n")),
            }
        }
    }
    out
}

/// `prompt check` 的输出与结论。
fn check_report(path: &Path) -> Result<(String, bool)> {
    let variant = load_variant(path).map_err(anyhow::Error::msg)?;
    let mut out = format!(
        "variant {} → {} / {}\n",
        variant.id, variant.role, variant.section
    );
    match check_variant(&variant) {
        Ok(check) => {
            out.push_str(&format!(
                "bundle {} → {}\n",
                check.base_bundle, check.candidate_bundle
            ));
            out.push_str(&line_diff(&check.original, &variant.text));
            out.push_str("ok: passes the structure and safety gate\n");
            Ok((out, true))
        }
        Err(problems) => {
            for problem in problems {
                out.push_str(&format!("error: {problem}\n"));
            }
            Ok((out, false))
        }
    }
}

/// 行级差异：只列删掉与新增的行（段落很短，够看清改了哪条规则）。
fn line_diff(before: &str, after: &str) -> String {
    let old: Vec<&str> = before.lines().collect();
    let new: Vec<&str> = after.lines().collect();
    let mut out = String::new();
    for line in &old {
        if !new.contains(line) {
            out.push_str(&format!("- {line}\n"));
        }
    }
    for line in &new {
        if !old.contains(line) {
            out.push_str(&format!("+ {line}\n"));
        }
    }
    out
}

/// 候选里的 `target.section` 不一定是提示词段名（例如 `tool:edit_file:description`
/// 或 `review`）：映射到最接近的段，映射不了就报错让人指定。
fn section_for(role: &PromptRole, target_section: &str) -> Option<&'static str> {
    let wanted = match (role, target_section) {
        (PromptRole::Main, section) if section.starts_with("tool") => "tool_rules",
        (PromptRole::Main, "boundary") => "tool_rules",
        (PromptRole::Main, "goal_continuation" | "review") => "delegation",
        (PromptRole::Worker(_), section) if section.starts_with("tool") => "capability_prompt",
        (PromptRole::Worker(_), "boundary") => "boundary",
        (_, section) => section,
    };
    role.sections()
        .iter()
        .copied()
        .find(|section| *section == wanted)
}

fn draft_from_candidates(path: &Path, index: usize) -> Result<PromptVariant> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let file: serde_json::Value =
        serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    let candidates = file
        .get("candidates")
        .and_then(|value| value.as_array())
        .with_context(|| format!("{} has no candidates array", path.display()))?;
    let candidate = candidates.get(index).with_context(|| {
        format!(
            "{} has {} candidate(s); index {index} is out of range",
            path.display(),
            candidates.len()
        )
    })?;
    let field = |pointer: &str| {
        candidate
            .pointer(pointer)
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_owned()
    };
    let target_role = field("/target/role");
    // 工具候选的角色可能是 `tool:<名>`：工具说明在主 Agent 的工具规则里。
    let role = PromptRole::parse(&target_role)
        .or_else(|| target_role.starts_with("tool:").then_some(PromptRole::Main))
        .with_context(|| format!("candidate {index} targets an unknown role {target_role:?}"))?;
    let target_section = field("/target/section");
    let section = section_for(&role, &target_section).with_context(|| {
        format!(
            "candidate {index} targets section {target_section:?}, which does not map to a {} section ({})",
            role.name(),
            role.sections().join(", ")
        )
    })?;
    let original = base_section(&role, section)
        .with_context(|| format!("{} does not send {section}", role.name()))?;
    let signal = field("/signal");
    let id: String = format!("{}-{}", role.name(), signal)
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    Ok(PromptVariant {
        schema: VARIANT_SCHEMA.to_owned(),
        id,
        role: role.name(),
        section: section.to_owned(),
        parent_bundle: role_bundle(&role),
        text: original,
        reason: field("/suggestion"),
        expected_effect: String::new(),
        risk: String::new(),
        source: Some(serde_json::json!({
            "signal": signal,
            "target": candidate.get("target"),
            "evidence": candidate.get("evidence"),
        })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("willdeep-prompt-cmd-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn sections_list_every_role() {
        let text = render_sections(&PromptRole::all());
        assert!(text.contains("main  main@"));
        assert!(text.contains("  tool_rules"));
        assert!(text.contains("input_suggestion  input_suggestion@"));
        assert!(text.contains("worker:reviewer  worker:reviewer@"));
    }

    #[test]
    fn a_draft_from_a_candidate_checks_once_its_text_changes() {
        let candidates = temp("candidates.json");
        std::fs::write(
            &candidates,
            serde_json::json!({
                "generated_at": "2026-09-30T00:00:00.000Z",
                "candidates": [{
                    "target": {"role": "main", "bundle": null, "section": "tool_rules:edit"},
                    "signal": "tool_failed:edit_file/edit_text_not_found",
                    "evidence": {"count": 6, "rate": null, "examples": []},
                    "suggestion": "strengthen the read-before-edit rule"
                }]
            })
            .to_string(),
        )
        .unwrap();
        let mut draft = draft_from_candidates(&candidates, 0).unwrap();
        assert_eq!(draft.role, "main");
        assert_eq!(draft.section, "tool_rules");
        assert_eq!(draft.id, "main-tool-failed-edit-file-edit-text-not-found");
        assert_eq!(draft.reason, "strengthen the read-before-edit rule");
        assert!(draft_from_candidates(&candidates, 1).is_err());

        let path = candidates.with_file_name("variant.json");
        std::fs::write(&path, serde_json::to_string(&draft).unwrap()).unwrap();
        let (report, valid) = check_report(&path).unwrap();
        assert!(!valid && report.contains("identical"), "{report}");

        draft.text.push_str(
            "\n- Before edit_file, re-read the exact lines you are replacing in this turn.",
        );
        std::fs::write(&path, serde_json::to_string(&draft).unwrap()).unwrap();
        let (report, valid) = check_report(&path).unwrap();
        assert!(valid, "{report}");
        assert!(report.contains("+ - Before edit_file, re-read"), "{report}");
        assert!(report.contains("ok: passes"));
        let _ = std::fs::remove_dir_all(candidates.parent().unwrap());
    }
}
