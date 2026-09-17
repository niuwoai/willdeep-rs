//! `willdeep handoff`：接住 WillDeep for macOS 用 `/handoff` 交过来的会话。
//!
//! 信道是 git 远端本身。Mac 端把「HEAD + 未提交改动」连同两份传输文件推到
//! `willdeep/handoff/*` 分支：
//!
//! - `.willdeep/handoff/<会话 id>/session.json`：Session v1 的对话、计划与目标；
//! - `.willdeep/handoff/<会话 id>/brief.md`：接手后的第一条提示。
//!
//! 这里只读分支、导入会话、切到分支，续跑交给 `run` 的原有路径（daemon /
//! 本地、审批、退出码都不另起一套）。两边机器各用各的 git 凭据；本命令不碰凭据。
//! 合同见 Xedit `docs/SESSION_COLLABORATION_AND_HANDOFF.md`。

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde_json::Value;
use uuid::Uuid;
use willdeep_core::conversation::Plan;
use willdeep_core::session::SESSION_VERSION;
use willdeep_core::types::sanitize_tool_history;
use willdeep_core::{Message, Session, SessionStore};

pub(crate) const BRANCH_PREFIX: &str = "willdeep/handoff/";
pub(crate) const TRANSPORT_DIRECTORY: &str = ".willdeep/handoff";
/// 交接会话文件的上限。Mac 端正文封顶 400 KB，这里留足余量但挡住异常大的 blob。
const MAX_TRANSPORT_BYTES: usize = 16 << 20;
const MIN_WATCH_INTERVAL_SECONDS: u64 = 10;
const FALLBACK_BRIEF: &str = "Continue the task that was handed off from WillDeep for macOS. Check the real state of the repository (git status, build, tests) before trusting earlier claims.";

#[derive(Clone, Debug, Subcommand)]
pub(crate) enum HandoffAction {
    /// List handoff branches on the remote and whether they were taken.
    List {
        /// Git remote to read. Defaults to origin.
        #[arg(long, default_value = "origin")]
        remote: String,
        /// Emit one JSON array instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Check out a handoff branch, import its session and continue the task.
    Accept(AcceptArgs),
    /// Poll the remote for new handoffs; optionally accept them one by one.
    Watch {
        /// Git remote to poll. Defaults to origin.
        #[arg(long, default_value = "origin")]
        remote: String,
        /// Seconds between polls (minimum 10).
        #[arg(long, default_value_t = 60)]
        interval: u64,
        /// Accept each new pending handoff and run it to completion before polling again.
        #[arg(long)]
        accept: bool,
    },
}

#[derive(Clone, Debug, Args)]
pub(crate) struct AcceptArgs {
    /// Handoff branch, with or without the willdeep/handoff/ prefix. Newest pending when omitted.
    pub(crate) branch: Option<String>,
    /// Git remote to read. Defaults to origin.
    #[arg(long, default_value = "origin")]
    pub(crate) remote: String,
    /// Import and check out only; print the command that continues the session.
    #[arg(long)]
    pub(crate) no_run: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HandoffStatus {
    /// Transport files present and not imported on this machine.
    Pending,
    /// This machine already imported the session.
    Imported,
    /// The branch head no longer carries transport files: someone took it.
    Taken,
}

impl HandoffStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Imported => "imported",
            Self::Taken => "taken",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HandoffEntry {
    pub(crate) branch: String,
    pub(crate) commit: String,
    pub(crate) committed_at: u64,
    pub(crate) subject: String,
    pub(crate) session_id: Option<Uuid>,
    pub(crate) status: HandoffStatus,
}

/// What `run` needs to continue an accepted handoff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreparedRun {
    pub(crate) session_id: Uuid,
    pub(crate) brief_path: PathBuf,
    pub(crate) workspace: PathBuf,
}

pub(crate) async fn run(
    action: HandoffAction,
    home: &Path,
    workspace: Option<&Path>,
    forwarded_global_args: Vec<OsString>,
) -> Result<()> {
    match action {
        HandoffAction::List { remote, json } => {
            let git = Git::discover(workspace)?;
            git.fetch_handoffs(&remote)?;
            print_entries(&git.entries(&remote, &SessionStore::new(home))?, json)
        }
        HandoffAction::Accept(args) => {
            // `main` turns accept-with-run into a `run`; only --no-run lands here.
            accept(&args, home, workspace).map(|_| ())
        }
        HandoffAction::Watch {
            remote,
            interval,
            accept,
        } => {
            watch(
                home,
                workspace,
                &remote,
                interval,
                accept,
                forwarded_global_args,
            )
            .await
        }
    }
}

/// Imports the chosen handoff and checks out its branch. Returns what `run`
/// needs, or `None` with `--no-run`.
pub(crate) fn accept(
    args: &AcceptArgs,
    home: &Path,
    workspace: Option<&Path>,
) -> Result<Option<PreparedRun>> {
    let git = Git::discover(workspace)?;
    let store = SessionStore::new(home);
    git.fetch_handoffs(&args.remote)?;
    let entries = git.entries(&args.remote, &store)?;
    let entry = choose(&entries, args.branch.as_deref(), &args.remote)?;
    let session_id = entry
        .session_id
        .context("pending handoff without a session id")?;

    if !git
        .run(&["status", "--porcelain", "--untracked-files=no"])?
        .trim()
        .is_empty()
    {
        bail!(
            "{} has uncommitted changes; commit or stash them before accepting a handoff",
            git.root.display()
        );
    }
    git.check_out(&args.remote, &entry.branch)?;

    let session_path = transport_path(session_id, "session.json");
    let raw = git.show_bytes(&entry.commit, &session_path)?;
    let mut session = build_session(&raw, session_id, git.root.clone())?;
    let brief = git
        .show_bytes(&entry.commit, &transport_path(session_id, "brief.md"))
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| FALLBACK_BRIEF.to_owned());
    store
        .save(&mut session)
        .with_context(|| format!("import handoff session {session_id}"))?;

    let brief_directory = home.join("handoff").join(session_id.to_string());
    std::fs::create_dir_all(&brief_directory)
        .with_context(|| format!("create {}", brief_directory.display()))?;
    let brief_path = brief_directory.join("brief.md");
    std::fs::write(&brief_path, brief)
        .with_context(|| format!("write {}", brief_path.display()))?;

    eprintln!(
        "imported handoff {} as session {session_id} ({}) on branch {}",
        entry.commit.get(..12).unwrap_or(&entry.commit),
        session.title,
        entry.branch
    );
    if args.no_run {
        println!("continue with: willdeep -r {session_id}");
        return Ok(None);
    }
    Ok(Some(PreparedRun {
        session_id,
        brief_path,
        workspace: git.root.clone(),
    }))
}

/// Picks the requested branch, or the newest pending one.
pub(crate) fn choose<'a>(
    entries: &'a [HandoffEntry],
    requested: Option<&str>,
    remote: &str,
) -> Result<&'a HandoffEntry> {
    let Some(requested) = requested else {
        return entries
            .iter()
            .find(|entry| entry.status == HandoffStatus::Pending)
            .with_context(|| format!("no pending handoff on {remote}"));
    };
    let branch = normalize_branch(requested);
    // Exact name first; otherwise a unique fragment such as the title slug,
    // because nobody retypes the timestamp part by hand.
    let entry = match entries.iter().find(|entry| entry.branch == branch) {
        Some(entry) => entry,
        None => {
            let fragment = requested.trim().trim_start_matches(BRANCH_PREFIX);
            let matches: Vec<&HandoffEntry> = entries
                .iter()
                .filter(|entry| {
                    !fragment.is_empty()
                        && entry
                            .branch
                            .strip_prefix(BRANCH_PREFIX)
                            .is_some_and(|name| name.contains(fragment))
                })
                .collect();
            match matches.as_slice() {
                [only] => *only,
                [] => bail!("no handoff branch matching {requested} on {remote}"),
                many => bail!(
                    "{requested} matches {} handoff branches: {}",
                    many.len(),
                    many.iter()
                        .map(|entry| entry.branch.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }
        }
    };
    let branch = entry.branch.clone();
    match entry.status {
        HandoffStatus::Pending => Ok(entry),
        HandoffStatus::Imported => bail!(
            "{branch} was already imported as session {}; continue with: willdeep -r {}",
            entry
                .session_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
            entry
                .session_id
                .map(|id| id.to_string())
                .unwrap_or_default()
        ),
        HandoffStatus::Taken => {
            bail!("{branch} no longer carries handoff files; it was already taken")
        }
    }
}

pub(crate) fn normalize_branch(value: &str) -> String {
    let trimmed = value.trim().trim_start_matches("refs/heads/");
    if trimmed.starts_with(BRANCH_PREFIX) {
        trimmed.to_owned()
    } else {
        format!("{BRANCH_PREFIX}{trimmed}")
    }
}

fn transport_path(session_id: Uuid, file: &str) -> String {
    format!("{TRANSPORT_DIRECTORY}/{session_id}/{file}")
}

/// Builds a fresh local session from the transport JSON. Only the
/// conversation, plan, goal and title are taken: paths, profile, model and
/// config in a file someone pushed must never steer this machine.
pub(crate) fn build_session(raw: &[u8], expected_id: Uuid, workspace: PathBuf) -> Result<Session> {
    let value: Value = serde_json::from_slice(raw).context("handoff session.json is not JSON")?;
    let version = value.get("version").and_then(Value::as_u64);
    if version != Some(u64::from(SESSION_VERSION)) {
        bail!("unsupported handoff session version {version:?}; expected {SESSION_VERSION}");
    }
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .and_then(|id| Uuid::parse_str(id).ok())
        .context("handoff session.json has no valid id")?;
    if id != expected_id {
        bail!("handoff session id {id} does not match its folder {expected_id}");
    }
    let mut messages: Vec<Message> = serde_json::from_value(
        value
            .get("messages")
            .cloned()
            .unwrap_or(Value::Array(Vec::new())),
    )
    .context("handoff session.json has malformed messages")?;
    sanitize_tool_history(&mut messages);
    let plan: Option<Plan> = match value.get("current_plan") {
        None | Some(Value::Null) => None,
        Some(plan) => Some(
            serde_json::from_value(plan.clone())
                .context("handoff session.json has a malformed plan")?,
        ),
    };
    let goal = value
        .get("goal")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|goal| !goal.is_empty())
        .map(str::to_owned);

    let mut session = Session::new(workspace, None, "");
    session.id = id;
    if let Some(title) = value
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
    {
        session.title = title.chars().take(200).collect();
    }
    session.messages = messages;
    session.current_plan = plan;
    session.goal = goal;
    Ok(session)
}

fn print_entries(entries: &[HandoffEntry], json: bool) -> Result<()> {
    if json {
        let rows: Vec<Value> = entries
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "branch": entry.branch,
                    "commit": entry.commit,
                    "committed_at": entry.committed_at,
                    "subject": entry.subject,
                    "session_id": entry.session_id,
                    "status": entry.status.label(),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if entries.is_empty() {
        println!("no handoff branches");
        return Ok(());
    }
    for entry in entries {
        println!(
            "{:<9} {:<60} {}",
            entry.status.label(),
            entry.branch,
            entry.subject.trim_start_matches("chore(handoff): ")
        );
    }
    Ok(())
}

async fn watch(
    home: &Path,
    workspace: Option<&Path>,
    remote: &str,
    interval: u64,
    accept: bool,
    forwarded_global_args: Vec<OsString>,
) -> Result<()> {
    let git = Git::discover(workspace)?;
    let interval = Duration::from_secs(interval.max(MIN_WATCH_INTERVAL_SECONDS));
    let mut seen: HashSet<(String, String)> = HashSet::new();
    eprintln!(
        "watching {remote} for handoffs in {} every {}s (Ctrl-C to stop)",
        git.root.display(),
        interval.as_secs()
    );
    loop {
        let scan = git
            .fetch_handoffs(remote)
            .and_then(|()| git.entries(remote, &SessionStore::new(home)));
        match scan {
            Ok(entries) => {
                // Oldest first: work handed off earlier is continued earlier.
                for entry in entries.iter().rev() {
                    if entry.status != HandoffStatus::Pending
                        || !seen.insert((entry.branch.clone(), entry.commit.clone()))
                    {
                        continue;
                    }
                    println!("new handoff: {} {}", entry.branch, entry.subject);
                    if accept {
                        let status = tokio::process::Command::new(std::env::current_exe()?)
                            .args(&forwarded_global_args)
                            .arg("--workspace")
                            .arg(&git.root)
                            .args([
                                "handoff",
                                "accept",
                                entry.branch.as_str(),
                                "--remote",
                                remote,
                            ])
                            .status()
                            .await
                            .context("start willdeep handoff accept")?;
                        println!("handoff {} finished: {status}", entry.branch);
                    }
                }
            }
            Err(error) => eprintln!("warning: handoff scan failed: {error:#}"),
        }
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            () = tokio::time::sleep(interval) => {}
        }
    }
}

pub(crate) struct Git {
    pub(crate) root: PathBuf,
}

impl Git {
    pub(crate) fn discover(workspace: Option<&Path>) -> Result<Self> {
        let start = match workspace {
            Some(path) => path.to_path_buf(),
            None => std::env::current_dir().context("read current directory")?,
        };
        let probe = Self {
            root: start.clone(),
        };
        let root = probe
            .run(&["rev-parse", "--show-toplevel"])
            .with_context(|| format!("{} is not inside a git repository", start.display()))?;
        let root = PathBuf::from(root.trim());
        Ok(Self {
            root: root.canonicalize().unwrap_or(root),
        })
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new("git");
        command
            .args(["-c", "core.quotePath=false"])
            .args(args)
            .current_dir(&self.root)
            .env("GIT_TERMINAL_PROMPT", "0");
        command
    }

    pub(crate) fn run(&self, args: &[&str]) -> Result<String> {
        let bytes = self.run_bytes(args)?;
        String::from_utf8(bytes)
            .with_context(|| format!("git {} printed non-UTF-8 output", args.join(" ")))
    }

    fn run_bytes(&self, args: &[&str]) -> Result<Vec<u8>> {
        let output = self
            .command(args)
            .output()
            .with_context(|| format!("start git {}", args.join(" ")))?;
        if !output.status.success() {
            bail!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(output.stdout)
    }

    pub(crate) fn fetch_handoffs(&self, remote: &str) -> Result<()> {
        let refspec =
            format!("+refs/heads/{BRANCH_PREFIX}*:refs/remotes/{remote}/{BRANCH_PREFIX}*");
        self.run(&["fetch", "--quiet", "--prune", remote, &refspec])
            .map(|_| ())
            .with_context(|| format!("fetch handoff branches from {remote}; this machine needs git credentials for it"))
    }

    /// Handoff branches already fetched from `remote`, newest first.
    pub(crate) fn entries(&self, remote: &str, store: &SessionStore) -> Result<Vec<HandoffEntry>> {
        let namespace = format!("refs/remotes/{remote}/{BRANCH_PREFIX}");
        let listing = self.run(&[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname)%09%(objectname)%09%(committerdate:unix)%09%(contents:subject)",
            &namespace,
        ])?;
        let strip = format!("refs/remotes/{remote}/");
        let mut entries = Vec::new();
        for line in listing.lines() {
            let mut fields = line.splitn(4, '\t');
            let (Some(reference), Some(commit), Some(time), subject) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let Some(branch) = reference.strip_prefix(&strip) else {
                continue;
            };
            let session_id = self.transport_session_id(commit)?;
            let status = match session_id {
                None => HandoffStatus::Taken,
                Some(id) if store.load(id).is_ok() => HandoffStatus::Imported,
                Some(_) => HandoffStatus::Pending,
            };
            entries.push(HandoffEntry {
                branch: branch.to_owned(),
                commit: commit.to_owned(),
                committed_at: time.parse().unwrap_or_default(),
                subject: subject.unwrap_or_default().to_owned(),
                session_id,
                status,
            });
        }
        Ok(entries)
    }

    fn transport_session_id(&self, commit: &str) -> Result<Option<Uuid>> {
        let files = self.run(&[
            "ls-tree",
            "-r",
            "--name-only",
            commit,
            "--",
            TRANSPORT_DIRECTORY,
        ])?;
        Ok(files.lines().find_map(|path| {
            let rest = path.strip_prefix(TRANSPORT_DIRECTORY)?.strip_prefix('/')?;
            let (id, file) = rest.split_once('/')?;
            (file == "session.json")
                .then(|| Uuid::parse_str(id).ok())
                .flatten()
        }))
    }

    fn show_bytes(&self, commit: &str, path: &str) -> Result<Vec<u8>> {
        let object = format!("{commit}:{path}");
        let size: usize = self
            .run(&["cat-file", "-s", &object])?
            .trim()
            .parse()
            .with_context(|| format!("size of {object}"))?;
        if size > MAX_TRANSPORT_BYTES {
            bail!("{path} is {size} bytes; handoff files are limited to {MAX_TRANSPORT_BYTES}");
        }
        self.run_bytes(&["cat-file", "blob", &object])
    }

    fn check_out(&self, remote: &str, branch: &str) -> Result<()> {
        let tracking = format!("{remote}/{branch}");
        let local = format!("refs/heads/{branch}");
        let exists = self
            .command(&["rev-parse", "--verify", "--quiet", &local])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        if exists {
            self.run(&["switch", "--quiet", branch])?;
            self.run(&["merge", "--ff-only", "--quiet", &tracking])
                .with_context(|| format!("local {branch} has diverged from {tracking}"))?;
        } else {
            self.run(&["switch", "--quiet", "-c", branch, "--track", &tracking])?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use willdeep_core::Role;

    struct Sandbox {
        root: PathBuf,
        remote: PathBuf,
        sender: PathBuf,
        receiver: PathBuf,
        home: PathBuf,
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn git(directory: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=Handoff Test",
                "-c",
                "user.email=handoff@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .current_dir(directory)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("utf8")
    }

    /// The shape WillDeep for macOS writes (`WillDeepCLIHandoff.cliSessionJSON`).
    fn mac_session_json(id: Uuid) -> String {
        serde_json::json!({
            "version": 1,
            "id": id.to_string(),
            "title": "修复登录 Login flow",
            "title_source": "legacy",
            "workspace": ".",
            "profile": null,
            "config": "/Users/someone/evil.toml",
            "created_at": 100,
            "updated_at": 100,
            "messages": [
                {"role": "user", "content": "把 app.txt 改成 v2"},
                {"role": "user", "content": "计划已批准", "source": "hostInstruction"},
                {"role": "tool", "content": "orphan", "tool_call_id": "call-x"},
                {"role": "assistant", "content": "改完了", "reasoning": "先看文件"}
            ],
            "current_plan": {"summary": "", "steps": [
                {"id": "a", "text": "改 app.txt", "status": "done"},
                {"id": "b", "text": "跑测试", "status": "in_progress"}
            ]},
            "goal": "登录不再超时"
        })
        .to_string()
    }

    fn sandbox() -> Sandbox {
        let root = std::env::temp_dir().join(format!("willdeep-handoff-{}", Uuid::new_v4()));
        let remote = root.join("remote.git");
        let sender = root.join("sender");
        let receiver = root.join("receiver");
        let home = root.join("home");
        for directory in [&remote, &sender, &home] {
            std::fs::create_dir_all(directory).expect("create dir");
        }
        git(&remote, &["init", "--quiet", "--bare"]);
        git(&sender, &["init", "--quiet"]);
        std::fs::write(sender.join("app.txt"), "v1\n").expect("write");
        git(&sender, &["add", "."]);
        git(&sender, &["commit", "--quiet", "-m", "init"]);
        git(
            &sender,
            &["remote", "add", "origin", remote.to_str().expect("path")],
        );
        git(&sender, &["push", "--quiet", "origin", "main"]);
        git(
            &root,
            &[
                "clone",
                "--quiet",
                remote.to_str().expect("path"),
                "receiver",
            ],
        );
        Sandbox {
            root,
            remote,
            sender,
            receiver,
            home,
        }
    }

    fn push_handoff(sandbox: &Sandbox, branch: &str, id: Uuid) {
        git(&sandbox.sender, &["switch", "--quiet", "-c", branch]);
        std::fs::write(sandbox.sender.join("app.txt"), "v2\n").expect("write");
        let directory = sandbox
            .sender
            .join(TRANSPORT_DIRECTORY)
            .join(id.to_string());
        std::fs::create_dir_all(&directory).expect("create transport dir");
        std::fs::write(directory.join("session.json"), mac_session_json(id)).expect("write");
        std::fs::write(directory.join("brief.md"), "# Handoff\n继续跑测试\n").expect("write");
        git(&sandbox.sender, &["add", "-A"]);
        git(
            &sandbox.sender,
            &["commit", "--quiet", "-m", "chore(handoff): 修复登录"],
        );
        git(&sandbox.sender, &["push", "--quiet", "origin", branch]);
        git(&sandbox.sender, &["switch", "--quiet", "main"]);
    }

    #[test]
    fn builds_a_clean_session_from_mac_transport_json() {
        let id = Uuid::new_v4();
        let session = build_session(mac_session_json(id).as_bytes(), id, PathBuf::from("/repo"))
            .expect("build");
        assert_eq!(session.id, id);
        assert_eq!(session.title, "修复登录 Login flow");
        assert_eq!(session.workspace, PathBuf::from("/repo"));
        // A pushed file must not choose this machine's config.
        assert_eq!(session.config, None);
        assert_eq!(session.goal.as_deref(), Some("登录不再超时"));
        assert_eq!(
            session.current_plan.as_ref().map(|plan| plan.steps.len()),
            Some(2)
        );
        let roles: Vec<Role> = session
            .messages
            .iter()
            .map(|message| message.role.clone())
            .collect();
        // The orphan tool result is dropped by sanitize_tool_history.
        assert_eq!(roles, vec![Role::User, Role::User, Role::Assistant]);
        assert_eq!(session.messages[2].reasoning.as_deref(), Some("先看文件"));
    }

    #[test]
    fn rejects_wrong_version_and_mismatched_id() {
        let id = Uuid::new_v4();
        let wrong_version = mac_session_json(id).replace("\"version\":1", "\"version\":2");
        assert!(build_session(wrong_version.as_bytes(), id, PathBuf::from("/repo")).is_err());
        assert!(
            build_session(
                mac_session_json(id).as_bytes(),
                Uuid::new_v4(),
                PathBuf::from("/repo")
            )
            .is_err()
        );
        assert!(build_session(b"not json", id, PathBuf::from("/repo")).is_err());
    }

    #[test]
    fn branch_names_accept_short_and_full_forms() {
        assert_eq!(
            normalize_branch("20260917-x"),
            "willdeep/handoff/20260917-x"
        );
        assert_eq!(
            normalize_branch("willdeep/handoff/20260917-x"),
            "willdeep/handoff/20260917-x"
        );
        assert_eq!(
            normalize_branch(" refs/heads/willdeep/handoff/y "),
            "willdeep/handoff/y"
        );
    }

    #[test]
    fn lists_accepts_and_marks_handoffs() {
        let sandbox = sandbox();
        let id = Uuid::new_v4();
        let branch = "willdeep/handoff/20260917-120000-fix-login";
        push_handoff(&sandbox, branch, id);

        let receiver = Git::discover(Some(&sandbox.receiver)).expect("discover");
        let store = SessionStore::new(&sandbox.home);
        receiver.fetch_handoffs("origin").expect("fetch");
        let entries = receiver.entries("origin", &store).expect("entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].branch, branch);
        assert_eq!(entries[0].status, HandoffStatus::Pending);
        assert_eq!(entries[0].session_id, Some(id));
        assert_eq!(entries[0].subject, "chore(handoff): 修复登录");

        let args = AcceptArgs {
            branch: None,
            remote: "origin".to_owned(),
            no_run: false,
        };
        let prepared = accept(&args, &sandbox.home, Some(&sandbox.receiver))
            .expect("accept")
            .expect("prepared run");
        assert_eq!(prepared.session_id, id);
        assert_eq!(prepared.workspace, receiver.root);
        assert_eq!(
            std::fs::read_to_string(&prepared.brief_path).expect("brief"),
            "# Handoff\n继续跑测试\n"
        );
        assert_eq!(
            receiver
                .run(&["symbolic-ref", "--short", "HEAD"])
                .expect("head")
                .trim(),
            branch
        );
        assert_eq!(
            std::fs::read_to_string(sandbox.receiver.join("app.txt")).expect("app"),
            "v2\n"
        );
        let imported = store.load(id).expect("imported session");
        assert_eq!(imported.workspace, receiver.root);

        let again = receiver.entries("origin", &store).expect("entries");
        assert_eq!(again[0].status, HandoffStatus::Imported);
        let error = accept(
            &AcceptArgs {
                branch: Some(branch.to_owned()),
                ..args.clone()
            },
            &sandbox.home,
            Some(&sandbox.receiver),
        )
        .expect_err("already imported");
        assert!(format!("{error:#}").contains(&format!("willdeep -r {id}")));

        // Once the receiving agent removes the transport folder, a fresh
        // machine sees the branch as taken.
        git(
            &sandbox.receiver,
            &["rm", "-r", "--quiet", TRANSPORT_DIRECTORY],
        );
        git(
            &sandbox.receiver,
            &["commit", "--quiet", "-m", "remove handoff transport"],
        );
        git(&sandbox.receiver, &["push", "--quiet", "origin", branch]);
        let other_home = sandbox.root.join("other-home");
        receiver.fetch_handoffs("origin").expect("fetch");
        let taken = receiver
            .entries("origin", &SessionStore::new(&other_home))
            .expect("entries");
        assert_eq!(taken[0].status, HandoffStatus::Taken);
        assert_eq!(taken[0].session_id, None);
        assert!(sandbox.remote.exists());
    }

    #[test]
    fn refuses_a_dirty_tree_and_honours_no_run() {
        let sandbox = sandbox();
        let id = Uuid::new_v4();
        push_handoff(&sandbox, "willdeep/handoff/dirty", id);
        std::fs::write(sandbox.receiver.join("app.txt"), "local edit\n").expect("write");
        let args = AcceptArgs {
            branch: Some("dirty".to_owned()),
            remote: "origin".to_owned(),
            no_run: true,
        };
        let error = accept(&args, &sandbox.home, Some(&sandbox.receiver)).expect_err("dirty tree");
        assert!(format!("{error:#}").contains("uncommitted changes"));
        assert!(SessionStore::new(&sandbox.home).load(id).is_err());

        git(&sandbox.receiver, &["checkout", "--quiet", "--", "app.txt"]);
        let prepared = accept(&args, &sandbox.home, Some(&sandbox.receiver)).expect("accept");
        assert_eq!(prepared, None);
        assert!(SessionStore::new(&sandbox.home).load(id).is_ok());
    }

    #[test]
    fn choose_accepts_a_unique_fragment_and_rejects_ambiguity() {
        let entry = |branch: &str| HandoffEntry {
            branch: branch.to_owned(),
            commit: "abc".to_owned(),
            committed_at: 0,
            subject: String::new(),
            session_id: Some(Uuid::new_v4()),
            status: HandoffStatus::Pending,
        };
        let entries = vec![
            entry("willdeep/handoff/20260917-120000-fix-login"),
            entry("willdeep/handoff/20260917-130000-fix-logout"),
        ];
        assert_eq!(
            choose(&entries, Some("fix-login"), "origin")
                .expect("unique")
                .branch,
            "willdeep/handoff/20260917-120000-fix-login"
        );
        assert_eq!(
            choose(
                &entries,
                Some("willdeep/handoff/20260917-130000-fix-logout"),
                "origin"
            )
            .expect("exact")
            .branch,
            "willdeep/handoff/20260917-130000-fix-logout"
        );
        let ambiguous = choose(&entries, Some("fix-log"), "origin").expect_err("ambiguous");
        assert!(format!("{ambiguous:#}").contains("matches 2 handoff branches"));
        assert!(choose(&entries, Some("nothing"), "origin").is_err());
    }

    #[test]
    fn choose_without_pending_reports_the_remote() {
        let error = choose(&[], None, "origin").expect_err("nothing pending");
        assert_eq!(format!("{error:#}"), "no pending handoff on origin");
    }
}
