//! The worker's opening message. Everything the parent already knows is
//! compiled here — goal, facts, constraints, verifier, skill body and the
//! file contents themselves — so the worker never spends a turn re-finding
//! it, and oversized material is digested rather than silently dropped.

use std::path::Path;

use super::text::bounded;
use super::types::{SubagentProfile, TaskPacket};

/// Never inline more than this, whatever the window says.
const MAX_INLINE_BYTES: usize = 96 * 1024;

/// Bytes of relevant-file content a worker's first message may carry, as a
/// function of its window. Roughly three quarters of a token of budget per
/// token of window (≈1 token per 3 bytes of source), which lands a 32K worker
/// at 24 KB and a 64K worker at 48 KB — the largest single item it starts
/// with, and still leaving half the window for tool round trips and output.
fn inline_budget(context_window: u64) -> usize {
    usize::try_from(context_window.saturating_mul(3) / 4)
        .unwrap_or(MAX_INLINE_BYTES)
        .min(MAX_INLINE_BYTES)
}

/// Build the worker's first message: the packet the parent compiled, then the
/// free-text instruction. Relevant files are inlined here rather than left for
/// the worker to find — every grep it runs is window it does not get back.
pub(super) async fn compose_brief(
    prompt: &str,
    task: Option<&TaskPacket>,
    workspace: &Path,
    profile: &SubagentProfile,
    skills: Option<&crate::skills::SkillCatalog>,
) -> String {
    let Some(task) = task else {
        return prompt.to_owned();
    };
    let mut brief = String::new();
    brief.push_str(&format!("<goal>\n{}\n</goal>\n", task.goal.trim()));
    if let Some(name) = task
        .skill
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        // The skill body rides in the opening message like a relevant file:
        // a worker made to fetch its own instructions spends its window on
        // the fetching. Unresolvable skills are named, never silent — the
        // worker must not improvise the procedure it thinks it was given.
        let budget = inline_budget(profile.context_window) / 2;
        match skills.and_then(|catalog| catalog.read(name, None).ok()) {
            Some(body) => brief.push_str(&format!(
                "\n<skill name={name:?}>\n{}\n</skill>\n",
                bounded(body, budget)
            )),
            None => brief.push_str(&format!(
                "\n<skill name={name:?} status=\"unavailable: not installed on this runtime; say so in your report instead of improvising the procedure\" />\n"
            )),
        }
    }
    if !task.known_facts.is_empty() {
        brief.push_str("\n<known-facts>\n");
        for fact in &task.known_facts {
            brief.push_str(&format!("- {}\n", fact.trim()));
        }
        brief.push_str("</known-facts>\n");
    }
    if !task.constraints.is_empty() {
        brief.push_str("\n<constraints>\n");
        for constraint in &task.constraints {
            brief.push_str(&format!("- {}\n", constraint.trim()));
        }
        brief.push_str("</constraints>\n");
    }
    if let Some(verifier) = &task.verifier {
        brief.push_str(&format!(
            "\n<verifier command={:?} expected_exit_code=\"{}\">\nThe runtime runs this after you finish and again after every attempt. You do not decide whether you are done, and claiming success without it changes nothing.\n</verifier>\n",
            verifier.command,
            verifier.expected_exit_code()
        ));
    }
    let inline_files = task.inline_files();
    if !inline_files.is_empty() {
        let budget = inline_budget(profile.context_window);
        brief.push_str(
            &inline_relevant_files(
                workspace,
                &inline_files,
                budget,
                task.digest_oversized.unwrap_or(false),
                profile,
            )
            .await,
        );
    }
    brief.push_str(&format!(
        "\n<instruction>\n{}\n</instruction>\n",
        prompt.trim()
    ));
    brief
}

async fn inline_relevant_files(
    workspace: &Path,
    files: &[String],
    budget: usize,
    digest_oversized: bool,
    profile: &SubagentProfile,
) -> String {
    let mut rendered = String::from("\n<relevant-files>\n");
    let mut spent = 0usize;
    for path in files {
        let full = workspace.join(path);
        let Ok(content) = tokio::fs::read_to_string(&full).await else {
            rendered.push_str(&format!("<file path={path:?} status=\"unreadable\" />\n"));
            continue;
        };
        let remaining = budget.saturating_sub(spent);
        // Air-gapped degradation: material that does not fit the window gets
        // digested through the worker's own cheap model instead of dropped.
        // The digest is honest about what it is — a summary, not the file —
        // and the raw path stays named so the worker can still read slices.
        if digest_oversized && content.len() > remaining.max(1) {
            let digest = digest_material(profile, path, &content, remaining.min(budget / 4)).await;
            spent += digest.len();
            rendered.push_str(&digest);
            continue;
        }
        if remaining == 0 {
            rendered.push_str(&format!(
                "<file path={path:?} status=\"omitted: inline budget exhausted, read it yourself if you need it\" />\n"
            ));
            continue;
        }
        let truncated = content.len() > remaining;
        let slice = bounded(content, remaining);
        spent += slice.len();
        rendered.push_str(&format!(
            "<file path={path:?}{}>\n{slice}\n</file>\n",
            if truncated {
                " status=\"truncated\""
            } else {
                ""
            }
        ));
    }
    rendered.push_str("</relevant-files>\n");
    rendered
}

/// Map-reduce a file that does not fit the inline budget: shard it, summarize
/// each shard on the worker's own cheap model, and inline the digests.
///
/// This is the automated form of the air-gapped degradation ladder in
/// `docs/MODEL_TIERS.md`: when no long-context tier exists, oversized
/// material is sharded and reduced rather than silently dropped. Three
/// disciplines keep it honest:
///
/// - the result is *labeled* a digest, chunk by chunk, so nothing downstream
///   mistakes a summary for the file;
/// - identifiers, signatures and assertions are demanded verbatim — the same
///   rule the verifier failure digest lives by;
/// - a chunk whose summary call fails is reported failed, not skipped: a
///   digest with an unmarked hole reads as complete coverage.
const DIGEST_MAX_CHUNKS: usize = 4;

async fn digest_material(
    profile: &SubagentProfile,
    path: &str,
    content: &str,
    output_budget: usize,
) -> String {
    // Chunk on char boundaries, sized so every chunk fits the worker window
    // with room for the instruction and the reply.
    let chunk_bytes = (inline_budget(profile.context_window) / 2).max(4 * 1024);
    let mut chunks: Vec<&str> = Vec::new();
    let mut rest = content;
    while !rest.is_empty() && chunks.len() < DIGEST_MAX_CHUNKS {
        let mut cut = rest.len().min(chunk_bytes);
        while cut > 0 && !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        let (head, tail) = rest.split_at(cut);
        chunks.push(head);
        rest = tail;
    }
    let uncovered = !rest.is_empty();
    let per_chunk = (output_budget / chunks.len().max(1)).max(512);

    let mut rendered = format!(
        "<file path={path:?} status=\"digested: too large to inline, summarized in {} chunk(s) by {}\">\n",
        chunks.len(),
        profile.model.as_deref().unwrap_or("the worker model")
    );
    for (index, chunk) in chunks.iter().enumerate() {
        let request = crate::types::Message::user(format!(
            "Summarize this chunk ({} of {}) of file {path} for an engineer who cannot read the original. Preserve identifiers, function signatures, error messages and assertions verbatim. Be dense and factual; no advice.\n\n{chunk}",
            index + 1,
            chunks.len()
        ));
        match profile.provider.complete(&[request], &[]).await {
            Ok(completion) => rendered.push_str(&format!(
                "<chunk index=\"{}\">\n{}\n</chunk>\n",
                index + 1,
                bounded(completion.content, per_chunk)
            )),
            Err(error) => rendered.push_str(&format!(
                "<chunk index=\"{}\" status=\"digest failed: {error}\" />\n",
                index + 1
            )),
        }
    }
    if uncovered {
        rendered.push_str(&format!(
            "<uncovered note=\"content beyond {DIGEST_MAX_CHUNKS} chunks was not digested; read {path:?} directly for the remainder\" />\n"
        ));
    }
    rendered.push_str("</file>\n");
    rendered
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use super::*;
    use crate::background::BackgroundTaskRegistry;
    use crate::provider::{Provider, ProviderError};
    use crate::subagent::test_support::ReportProvider;
    use crate::subagent::types::{SpawnAgentArgs, TaskVerifier};
    use crate::subagent::{SubagentCatalog, builtin_profiles};
    use crate::types::{Completion, Message, ToolDefinition};

    /// A packet that names a skill gets the skill body inlined by the
    /// runtime; a skill this runtime does not have is named as unavailable —
    /// a worker must never improvise the procedure it thinks it was given.
    #[tokio::test]
    async fn a_named_skill_is_inlined_and_a_missing_one_is_called_out() {
        struct PromptProbe(Arc<Mutex<Vec<String>>>);

        #[async_trait]
        impl Provider for PromptProbe {
            async fn complete(
                &self,
                messages: &[Message],
                _tools: &[ToolDefinition],
            ) -> Result<Completion, ProviderError> {
                self.0
                    .lock()
                    .unwrap()
                    .push(messages.last().unwrap().content.clone());
                Ok(Completion {
                    content: "report".to_owned(),
                    tool_calls: Vec::new(),
                    finish_reason: Some("stop".to_owned()),
                    usage: None,
                })
            }
        }

        let root = std::env::temp_dir().join(format!("willdeep-skillpkt-{}", uuid::Uuid::new_v4()));
        let skill_dir = root.join(".willdeep/skills/convert");
        std::fs::create_dir_all(&skill_dir).expect("skill dir");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: convert\ndescription: convert images\ntier: worker\n---\n# Steps\nUse sips.",
        )
        .expect("skill");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let provider: Arc<dyn Provider> = Arc::new(PromptProbe(seen.clone()));
        let catalog = SubagentCatalog::new(
            &root,
            builtin_profiles(provider),
            Arc::new(BackgroundTaskRegistry::default()),
        )
        .with_skills(Arc::new(crate::skills::SkillCatalog::discover(&root, &[])));

        for (skill, marker) in [
            ("convert", "Use sips."),
            ("no-such-skill", "status=\"unavailable"),
        ] {
            catalog
                .run(
                    SpawnAgentArgs {
                        prompt: "do it".to_owned(),
                        profile: Some("scout".to_owned()),
                        run_in_background: Some(false),
                        task: Some(TaskPacket {
                            goal: "convert the asset".to_owned(),
                            skill: Some(skill.to_owned()),
                            ..TaskPacket::default()
                        }),
                        ..SpawnAgentArgs::default()
                    },
                    None,
                )
                .await
                .expect("run");
            let prompt = seen.lock().unwrap().last().cloned().unwrap();
            assert!(
                prompt.contains(marker),
                "skill {skill} should surface {marker}: {prompt}"
            );
        }
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// Oversized material with digestion on is sharded and summarized by the
    /// worker model, labeled as a digest — never silently dropped, never
    /// passed off as the file itself.
    #[tokio::test]
    async fn oversized_material_is_digested_not_dropped() {
        struct DigestProvider(Arc<Mutex<Vec<String>>>);

        #[async_trait]
        impl Provider for DigestProvider {
            async fn complete(
                &self,
                messages: &[Message],
                _tools: &[ToolDefinition],
            ) -> Result<Completion, ProviderError> {
                let content = messages.last().unwrap().content.clone();
                self.0.lock().unwrap().push(content.clone());
                Ok(Completion {
                    content: if content.contains("Summarize this chunk") {
                        "digest: the assertion `left == 42` fails".to_owned()
                    } else {
                        "report".to_owned()
                    },
                    tool_calls: Vec::new(),
                    finish_reason: Some("stop".to_owned()),
                    usage: None,
                })
            }
        }

        let root = std::env::temp_dir().join(format!("willdeep-digest-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("workspace");
        // Far past any inline budget for a deployable worker profile.
        std::fs::write(root.join("huge.log"), "x".repeat(64 * 1024)).expect("fixture");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let provider: Arc<dyn Provider> = Arc::new(DigestProvider(seen.clone()));
        let catalog = SubagentCatalog::new(
            &root,
            builtin_profiles(provider),
            Arc::new(BackgroundTaskRegistry::default()),
        );
        catalog
            .run(
                SpawnAgentArgs {
                    prompt: "explain".to_owned(),
                    profile: Some("log_inspector".to_owned()),
                    run_in_background: Some(false),
                    task: Some(TaskPacket {
                        goal: "explain the failure".to_owned(),
                        relevant_files: vec!["huge.log".to_owned()],
                        digest_oversized: Some(true),
                        ..TaskPacket::default()
                    }),
                    ..SpawnAgentArgs::default()
                },
                None,
            )
            .await
            .expect("run");
        let prompts = seen.lock().unwrap().clone();
        assert!(
            prompts.iter().any(|p| p.contains("Summarize this chunk")),
            "the digest path must actually call the model"
        );
        let brief = prompts
            .iter()
            .find(|p| p.contains("<relevant-files>"))
            .expect("worker brief");
        assert!(
            brief.contains("status=\"digested"),
            "the digest must be labeled a digest: {brief}"
        );
        assert!(
            brief.contains("left == 42"),
            "chunk digests must reach the brief"
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    /// The packet is the worker's whole starting position: goal, facts,
    /// constraints, verifier and the file contents themselves, so it never
    /// spends a turn re-finding what the parent already knew.
    #[tokio::test]
    async fn the_task_packet_inlines_what_the_parent_already_knows() {
        let root = std::env::temp_dir().join(format!("willdeep-brief-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("workspace");
        std::fs::write(root.join("target.rs"), "fn broken() {}\n").expect("fixture");
        let profile = builtin_profiles(Arc::new(ReportProvider) as Arc<dyn Provider>)
            .into_iter()
            .find(|profile| profile.id == "test_fixer")
            .expect("test_fixer profile");
        let brief = compose_brief(
            "fix it",
            Some(&TaskPacket {
                goal: "make testCrossSignOff pass".to_owned(),
                read_files: vec!["target.rs".to_owned(), "missing.rs".to_owned()],
                write_files: vec!["target.rs".to_owned()],
                relevant_files: Vec::new(),
                known_facts: vec!["broke at caba8df7".to_owned()],
                constraints: vec!["do not change the public API".to_owned()],
                verifier: Some(TaskVerifier {
                    command: "cargo test -p core".to_owned(),
                    expected_exit_code: None,
                }),
                max_attempts: None,
                skill: None,
                digest_oversized: None,
            }),
            &root,
            &profile,
            None,
        )
        .await;
        assert!(brief.contains("make testCrossSignOff pass"));
        assert!(brief.contains("broke at caba8df7"));
        assert!(brief.contains("do not change the public API"));
        assert!(brief.contains("cargo test -p core"));
        assert!(
            brief.contains("fn broken() {}"),
            "relevant files must arrive inlined, not as paths to go find: {brief}"
        );
        assert!(
            brief.contains("unreadable"),
            "a file that could not be read must be named as such, not silently dropped: {brief}"
        );
        assert!(brief.contains("<instruction>\nfix it\n</instruction>"));
        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
