//! Static command classifier: decides whether a shell command can be
//! auto-approved without bothering the user.
//!
//! This is the first pass of a two-tier approval policy modelled after the
//! macOS WillDeep (Xedit) implementation:
//!
//! 1. `CommandSafety::AlwaysSafe` — read-only or bounded workspace-create.
//!    Run it, no card, no round trip.
//! 2. `CommandSafety::AlwaysDangerous` — destructive shape. Never
//!    auto-approved, never sent to the AI judge; the user decides.
//! 3. `CommandSafety::NeedsJudgment` — unknown. Hand it to the AI judge
//!    (see [`crate::judge`]); if the judge is unavailable or says no, the
//!    user decides.
//!
//! The classifier is deliberately conservative: everything it cannot parse
//! falls back to `NeedsJudgment` (or `AlwaysDangerous` for shapes that
//! defeat parsing entirely), so a bug here costs an extra approval card
//! rather than an unreviewed `rm -rf`.

use std::collections::HashSet;
use std::sync::LazyLock;

/// Maximum `$(...)` nesting the expansion resolver will walk before giving up.
const MAX_SUBSTITUTION_DEPTH: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandSafety {
    AlwaysSafe,
    AlwaysDangerous,
    NeedsJudgment,
}

impl CommandSafety {
    /// Strictest verdict wins: dangerous > needs-judgment > safe.
    fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::AlwaysDangerous, _) | (_, Self::AlwaysDangerous) => Self::AlwaysDangerous,
            (Self::NeedsJudgment, _) | (_, Self::NeedsJudgment) => Self::NeedsJudgment,
            _ => Self::AlwaysSafe,
        }
    }
}

/// Commands whose entire effect is reading state — no filesystem writes, no
/// process signals, no external mutation. Matched on the basename of a
/// segment's head token.
static ALWAYS_SAFE_HEADS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        // navigation (shell state only)
        "cd",
        "pushd",
        "popd", // file inspection
        "cat",
        "head",
        "tail",
        "less",
        "more",
        "wc",
        "nl",
        "file",
        "stat",
        "du",
        "df",
        "od",
        "hexdump",
        "strings", // path helpers
        "realpath",
        "readlink",
        "dirname",
        "basename", // comparison
        "diff",
        "cmp", // listing
        "ls",
        "tree",
        "pwd", // search
        "rg",
        "ripgrep",
        "grep",
        "egrep",
        "fgrep",
        "ag",
        "fd", // stream transforms
        "uniq",
        "cut",
        "tr",
        "column",
        "paste",
        "join",
        "comm",
        "fold",
        "fmt",
        "expand",
        "unexpand",
        "rev",
        "tac", // hashing
        "md5",
        "md5sum",
        "shasum",
        "sha1sum",
        "sha256sum",
        "sha512sum",
        "cksum",
        "sum",
        // structured data (stdout only; file writes need `>`, handled separately)
        "jq", // environment / introspection
        "echo",
        "printf",
        "which",
        "whereis",
        "type",
        "printenv",
        "uname",
        "whoami",
        "id",
        "date",
        "uptime",
        "sw_vers",
        "arch",
        "hostname",
        "getconf",
        "locale",
        "groups",
        "logname",
        "man",
        "clear", // process / memory snapshots (no signals)
        "ps",
        "top",
        "lsof",
        "vm_stat",
        "free", // Spotlight metadata reads
        "mdfind",
        "mdls", // calculators
        "cal",
        "bc", // read-only network probes
        "ping",
        "host",
        "dig",
        "nslookup",
        "traceroute",
        "whois",
        // side-effect-free shell glue
        "read",
        "test",
        "[",
        "[[",
        "seq",
        "true",
        "false",
        "sleep",
        "expr",
    ]
    .into_iter()
    .collect()
});

/// Low-risk *write* commands that only create things. They mutate the
/// filesystem, so they are not read-only, but they carry no delete /
/// escalate / network semantics. Auto-approved only when the caller allows
/// workspace writes.
static SAFE_WORKSPACE_CREATE_HEADS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| ["mkdir", "touch", "mktemp"].into_iter().collect());

/// Token-level dangerous words. Every *unquoted* token of a segment is
/// checked, not just the head, so `find . | xargs rm` is caught by the `rm`
/// token in the second segment. Quoted tokens are data (`grep "rm" file`)
/// and are skipped.
static DANGEROUS_TOKENS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        // removal
        "rm",
        "rmdir",
        "shred", // privilege escalation
        "sudo",
        "doas",
        "su", // filesystem / machine lifecycle
        "mkfs",
        "shutdown",
        "reboot",
        "halt",
        "poweroff",
        // permission / ownership
        "chmod",
        "chown",
        "chgrp", // process signals
        "kill",
        "killall",
        "pkill", // move (destructive overwrite)
        "mv",    // disk / volume management
        "diskutil",
        "fdisk",
        "format", // launchd
        "launchctl",
    ]
    .into_iter()
    .collect()
});

/// Shapes that do not tokenize cleanly. Kept short on purpose — each entry
/// is a known one-off.
const DANGEROUS_SUBSTRINGS: &[&str] = &[":(){:|:&};:", ":(){ :|:& };:", "dd if=", "mkfs."];

/// `cp` / `ln` with any flag are the destructive shapes (recursive
/// overwrite, symlink replacement). Plain `cp src dst` still needs judgment.
static DANGEROUS_HEADS_WITH_FLAG: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| ["cp", "ln"].into_iter().collect());

/// Control-flow keywords that carry a real payload after them.
const PREFIX_CONTROL_KEYWORDS: &[&str] = &[
    "do", "then", "else", "elif", "if", "while", "until", "time", "!", "command", "builtin",
    "exec", "nohup",
];

/// Block-closing keywords with nothing to execute.
const BARE_CONTROL_KEYWORDS: &[&str] = &["done", "fi", "esac", "{", "}", "(", ")"];

static SAFE_GIT_SUBCOMMANDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "status",
        "log",
        "diff",
        "show",
        "blame",
        "describe",
        "shortlog",
        "whatchanged",
        "rev-parse",
        "rev-list",
        "ls-files",
        "ls-tree",
        "ls-remote",
        "cat-file",
        "for-each-ref",
        "symbolic-ref",
        "count-objects",
        "reflog",
        "grep",
        "annotate",
        "var",
        "help",
        "version",
    ]
    .into_iter()
    .collect()
});

/// Git subcommands that rewrite history or discard work without a prompt.
static DANGEROUS_GIT_SUBCOMMANDS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| ["filter-branch", "filter-repo"].into_iter().collect());

/// Cargo subcommands whose effects stay inside the workspace target dir.
static SAFE_CARGO_SUBCOMMANDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "check",
        "build",
        "b",
        "test",
        "t",
        "clippy",
        "fmt",
        "tree",
        "metadata",
        "doc",
        "bench",
        "nextest",
        "search",
        "verify-project",
        "locate-project",
        "version",
    ]
    .into_iter()
    .collect()
});

static SAFE_NODE_PM_SUBCOMMANDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    ["ls", "list", "view", "info", "outdated", "why", "audit"]
        .into_iter()
        .collect()
});

static SAFE_GO_SUBCOMMANDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "build", "test", "vet", "fmt", "list", "version", "env", "doc",
    ]
    .into_iter()
    .collect()
});

static SAFE_DOCKER_SUBCOMMANDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "ps", "images", "logs", "inspect", "version", "info", "port", "top", "stats",
    ]
    .into_iter()
    .collect()
});

/// Heads whose verdict depends on the argument shape.
static CONDITIONAL_HEADS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "git",
        "cargo",
        "go",
        "docker",
        "npm",
        "yarn",
        "pnpm",
        "find",
        "xargs",
        "sed",
        "awk",
        "sort",
        "tee",
        "curl",
        "wget",
        "make",
        "swift",
        "ruby",
        "rake",
        "python",
        "python3",
        "node",
        "kubectl",
        "systemctl",
        "journalctl",
        "ssh",
        "scp",
        "rsync",
        "mysql",
        "mariadb",
        "psql",
        "sqlite3",
        "defaults",
        "unzip",
        "tar",
        "zip",
        "base64",
        "iconv",
        "xxd",
        "yq",
        "sh",
        "bash",
        "zsh",
        "env",
    ]
    .into_iter()
    .collect()
});

#[derive(Clone, Debug, PartialEq, Eq)]
struct Token {
    text: String,
    /// Whether any part of the token came from inside quotes. Quoted text is
    /// data, never a command name.
    quoted: bool,
}

#[derive(Debug)]
struct Expansion {
    command: String,
    requires_judgment: bool,
}

/// Placeholder substituted for a `$(...)` whose payload proved read-only.
const SAFE_SUBSTITUTION_PLACEHOLDER: &str = "__willdeep_safe_substitution__";

/// Classify a shell command line.
pub fn classify(command: &str) -> CommandSafety {
    classify_inner(command, 0)
}

/// Classify with workspace-write allowance. When `allow_workspace_create` is
/// true, bounded creators (`mkdir`, `touch`, `mktemp`) count as safe.
pub fn classify_with_workspace_write(command: &str, allow_workspace_create: bool) -> CommandSafety {
    let verdict = classify_inner(command, 0);
    if allow_workspace_create {
        return verdict;
    }
    // Without workspace-write, a create-only command is not auto-safe.
    if verdict == CommandSafety::AlwaysSafe && contains_workspace_create_head(command) {
        return CommandSafety::NeedsJudgment;
    }
    verdict
}

fn contains_workspace_create_head(command: &str) -> bool {
    let Some(segments) = split_segments(command) else {
        return false;
    };
    segments.iter().any(|segment| {
        let tokens = strip_prefix_keywords(strip_env_assignments(tokenize(segment)));
        tokens
            .first()
            .is_some_and(|token| SAFE_WORKSPACE_CREATE_HEADS.contains(basename(&token.text)))
    })
}

fn classify_inner(command: &str, depth: usize) -> CommandSafety {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        // Never auto-run nothing.
        return CommandSafety::AlwaysDangerous;
    }
    if trimmed.contains("<<") {
        // Heredocs carry an unparsed body; let the judge or the user read it.
        return CommandSafety::NeedsJudgment;
    }
    let Some(expansion) = resolve_expansions(trimmed, depth) else {
        return CommandSafety::AlwaysDangerous;
    };
    // Inert stderr/dev-null redirections are removed before segmentation:
    // `2>&1` would otherwise be split on its `&` into a bogus `1` segment.
    let policy_command = strip_inert_redirections(&expansion.command);

    let Some(unquoted) = unquoted_text(&policy_command) else {
        return CommandSafety::NeedsJudgment;
    };

    // Fork bombs and friends span the whole line and would be shredded into
    // harmless-looking fragments by segment splitting. Scan first.
    let lowered = unquoted.to_ascii_lowercase();
    for needle in DANGEROUS_SUBSTRINGS {
        if lowered.contains(needle) {
            return CommandSafety::AlwaysDangerous;
        }
    }

    let mut verdict = if expansion.requires_judgment {
        CommandSafety::NeedsJudgment
    } else {
        CommandSafety::AlwaysSafe
    };

    if let Some(redirection) = redirection_verdict(&unquoted) {
        verdict = verdict.merge(redirection);
    }

    let Some(segments) = split_segments(&policy_command) else {
        return CommandSafety::NeedsJudgment;
    };
    if segments.is_empty() {
        return CommandSafety::AlwaysDangerous;
    }
    for segment in &segments {
        verdict = verdict.merge(classify_segment(segment));
        if verdict == CommandSafety::AlwaysDangerous {
            return verdict;
        }
    }
    verdict
}

/// Redirections that produce no observable state: merging stderr into stdout
/// and throwing output at `/dev/null`. Longest match first.
const INERT_REDIRECTIONS: &[&str] = &[
    "&>>/dev/null",
    "&>/dev/null",
    "2>>/dev/null",
    "2>/dev/null",
    ">>/dev/null",
    ">/dev/null",
    "2>&1",
    "1>&2",
    "2>&-",
];

/// Remove inert redirections that sit outside quotes, leaving quoted text
/// (`grep '2>&1' log`) untouched.
fn strip_inert_redirections(command: &str) -> String {
    let mut output = String::with_capacity(command.len());
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut rest = command;
    while !rest.is_empty() {
        if quote.is_none() && !escaped {
            let compact = rest.trim_start();
            if let Some(pattern) = INERT_REDIRECTIONS
                .iter()
                .find(|pattern| compact.starts_with(**pattern))
            {
                output.push(' ');
                rest = &compact[pattern.len()..];
                continue;
            }
        }
        let value = rest.chars().next().expect("non-empty remainder");
        rest = &rest[value.len_utf8()..];
        if escaped {
            output.push(value);
            escaped = false;
            continue;
        }
        if value == '\\' && quote != Some('\'') {
            output.push(value);
            escaped = true;
            continue;
        }
        if (value == '\'' || value == '"') && quote.is_none_or(|active| active == value) {
            quote = if quote.is_some() { None } else { Some(value) };
        }
        output.push(value);
    }
    output
}

/// Any surviving redirection needs review — the target could be outside the
/// workspace, or could clobber a file the operator cares about.
fn redirection_verdict(unquoted: &str) -> Option<CommandSafety> {
    unquoted
        .contains(['>', '<'])
        .then_some(CommandSafety::NeedsJudgment)
}

fn classify_segment(segment: &str) -> CommandSafety {
    let tokens = tokenize(segment);
    if tokens.is_empty() {
        return CommandSafety::AlwaysSafe;
    }

    // Dangerous tokens anywhere in the segment, but only when unquoted:
    // `grep "rm -rf" log` must stay a plain search.
    for token in &tokens {
        if !token.quoted && DANGEROUS_TOKENS.contains(basename(&token.text)) {
            return CommandSafety::AlwaysDangerous;
        }
    }

    let tokens = strip_prefix_keywords(strip_env_assignments(tokens));
    let Some(head_token) = tokens.first() else {
        // Nothing but assignments / keywords — no payload to run.
        return CommandSafety::AlwaysSafe;
    };
    if head_token.quoted {
        return CommandSafety::NeedsJudgment;
    }
    let head = basename(&head_token.text).to_ascii_lowercase();
    if BARE_CONTROL_KEYWORDS.contains(&head.as_str()) {
        return CommandSafety::AlwaysSafe;
    }
    let args = tokens
        .iter()
        .skip(1)
        .map(|token| token.text.as_str())
        .collect::<Vec<_>>();

    if DANGEROUS_HEADS_WITH_FLAG.contains(head.as_str()) {
        return if args.iter().any(|arg| arg.starts_with('-')) {
            CommandSafety::AlwaysDangerous
        } else {
            CommandSafety::NeedsJudgment
        };
    }
    if ALWAYS_SAFE_HEADS.contains(head.as_str()) {
        return CommandSafety::AlwaysSafe;
    }
    if SAFE_WORKSPACE_CREATE_HEADS.contains(head.as_str()) {
        return CommandSafety::AlwaysSafe;
    }
    if CONDITIONAL_HEADS.contains(head.as_str()) {
        return classify_conditional(&head, &args);
    }
    CommandSafety::NeedsJudgment
}

fn classify_conditional(head: &str, args: &[&str]) -> CommandSafety {
    let subcommand = args
        .iter()
        .find(|arg| !arg.starts_with('-'))
        .map(|arg| arg.to_ascii_lowercase());
    match head {
        "git" => classify_git(args, subcommand.as_deref()),
        "cargo" => match subcommand.as_deref() {
            Some(value) if SAFE_CARGO_SUBCOMMANDS.contains(value) => CommandSafety::AlwaysSafe,
            None if args.iter().any(|arg| *arg == "--version" || *arg == "-V") => {
                CommandSafety::AlwaysSafe
            }
            _ => CommandSafety::NeedsJudgment,
        },
        "go" => match subcommand.as_deref() {
            Some(value) if SAFE_GO_SUBCOMMANDS.contains(value) => CommandSafety::AlwaysSafe,
            _ => CommandSafety::NeedsJudgment,
        },
        "docker" => match subcommand.as_deref() {
            Some(value) if SAFE_DOCKER_SUBCOMMANDS.contains(value) => CommandSafety::AlwaysSafe,
            _ => CommandSafety::NeedsJudgment,
        },
        "npm" | "yarn" | "pnpm" => match subcommand.as_deref() {
            Some(value) if SAFE_NODE_PM_SUBCOMMANDS.contains(value) => CommandSafety::AlwaysSafe,
            _ => CommandSafety::NeedsJudgment,
        },
        "find" => {
            if args.iter().any(|arg| {
                matches!(
                    *arg,
                    "-delete"
                        | "-exec"
                        | "-execdir"
                        | "-ok"
                        | "-okdir"
                        | "-fls"
                        | "-fprint"
                        | "-fprintf"
                )
            }) {
                CommandSafety::NeedsJudgment
            } else {
                CommandSafety::AlwaysSafe
            }
        }
        "sed" => {
            if args
                .iter()
                .any(|arg| arg.starts_with("-i") || *arg == "--in-place")
            {
                CommandSafety::NeedsJudgment
            } else {
                CommandSafety::AlwaysSafe
            }
        }
        "awk" | "yq" => CommandSafety::AlwaysSafe,
        "sort" => {
            if args
                .iter()
                .any(|arg| *arg == "-o" || arg.starts_with("--output"))
            {
                CommandSafety::NeedsJudgment
            } else {
                CommandSafety::AlwaysSafe
            }
        }
        "base64" | "iconv" | "xxd" => CommandSafety::AlwaysSafe,
        "tar" | "unzip" | "zip" => {
            // Listing is read-only; extraction writes wherever it likes.
            if args
                .iter()
                .any(|arg| matches!(*arg, "-l" | "-t" | "-tf" | "-tzf" | "--list"))
            {
                CommandSafety::AlwaysSafe
            } else {
                CommandSafety::NeedsJudgment
            }
        }
        "defaults" => match subcommand.as_deref() {
            Some("read") | Some("read-type") | Some("domains") => CommandSafety::AlwaysSafe,
            _ => CommandSafety::NeedsJudgment,
        },
        "journalctl" | "kubectl" | "systemctl" => match subcommand.as_deref() {
            Some("get") | Some("describe") | Some("logs") | Some("status") | Some("list-units")
            | Some("top") => CommandSafety::AlwaysSafe,
            None => CommandSafety::AlwaysSafe,
            _ => CommandSafety::NeedsJudgment,
        },
        // Everything below reaches the network, another host, a database, or
        // an arbitrary interpreter. Bounded uses are common and legitimate,
        // so they go to the judge instead of straight to the user.
        _ => CommandSafety::NeedsJudgment,
    }
}

fn classify_git(args: &[&str], subcommand: Option<&str>) -> CommandSafety {
    let Some(subcommand) = subcommand else {
        return CommandSafety::AlwaysSafe;
    };
    if DANGEROUS_GIT_SUBCOMMANDS.contains(subcommand) {
        return CommandSafety::AlwaysDangerous;
    }
    let forceful = args
        .iter()
        .any(|arg| matches!(*arg, "-f" | "--force" | "--hard" | "--force-with-lease"));
    match subcommand {
        "push" if forceful => CommandSafety::AlwaysDangerous,
        "reset" if forceful => CommandSafety::AlwaysDangerous,
        "clean"
            if args
                .iter()
                .any(|arg| arg.starts_with("-") && arg.contains('f')) =>
        {
            CommandSafety::AlwaysDangerous
        }
        "checkout" | "switch" | "restore" if forceful => CommandSafety::NeedsJudgment,
        value if SAFE_GIT_SUBCOMMANDS.contains(value) => CommandSafety::AlwaysSafe,
        // `branch`/`tag`/`remote`/`stash`/`config` are read-only when they
        // only list; the mutating forms take a name argument or a flag.
        "branch" | "tag" | "remote" | "stash" | "config" | "worktree" => {
            let mutating = args.iter().skip(1).any(|arg| {
                !arg.starts_with('-')
                    && !matches!(
                        *arg,
                        "list" | "show" | "get" | "get-all" | "get-regexp" | "-l" | "--list"
                    )
            });
            if mutating {
                CommandSafety::NeedsJudgment
            } else {
                CommandSafety::AlwaysSafe
            }
        }
        _ => CommandSafety::NeedsJudgment,
    }
}

// MARK: - Shell parsing

/// Text outside single/double quotes, with quoted spans replaced by spaces.
/// Returns `None` when quoting is unbalanced.
fn unquoted_text(command: &str) -> Option<String> {
    let mut output = String::with_capacity(command.len());
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for value in command.chars() {
        if escaped {
            output.push(' ');
            escaped = false;
            continue;
        }
        if value == '\\' && quote != Some('\'') {
            escaped = true;
            output.push(' ');
            continue;
        }
        match quote {
            Some(active) if value == active => {
                quote = None;
                output.push(' ');
            }
            Some(_) => output.push(' '),
            None if value == '\'' || value == '"' => {
                quote = Some(value);
                output.push(' ');
            }
            None => output.push(value),
        }
    }
    (quote.is_none() && !escaped).then_some(output)
}

/// Replace `$(...)` whose payload is provably read-only with a placeholder so
/// the outer chain can be parsed, and report that the whole command still
/// needs judgment. Backticks, process substitution, and arithmetic expansion
/// return `None` (treated as dangerous by the caller).
fn resolve_expansions(command: &str, depth: usize) -> Option<Expansion> {
    if depth > MAX_SUBSTITUTION_DEPTH {
        return None;
    }
    let chars = command.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(command.len());
    let mut requires_judgment = false;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut index = 0;
    while index < chars.len() {
        let value = chars[index];
        if escaped {
            output.push(value);
            escaped = false;
            index += 1;
            continue;
        }
        if value == '\\' && quote != Some('\'') {
            output.push(value);
            escaped = true;
            index += 1;
            continue;
        }
        if value == '\'' && quote != Some('"') {
            quote = if quote == Some('\'') {
                None
            } else {
                Some('\'')
            };
            output.push(value);
            index += 1;
            continue;
        }
        if value == '"' && quote != Some('\'') {
            quote = if quote == Some('"') { None } else { Some('"') };
            output.push(value);
            index += 1;
            continue;
        }
        if quote != Some('\'') {
            if value == '`' {
                return None;
            }
            if (value == '<' || value == '>') && chars.get(index + 1) == Some(&'(') {
                return None;
            }
            if value == '$' && chars.get(index + 1) == Some(&'(') {
                if chars.get(index + 2) == Some(&'(') {
                    // Arithmetic expansion — not modelled.
                    return None;
                }
                let close = closing_parenthesis(&chars, index + 1)?;
                let inner = chars[index + 2..close].iter().collect::<String>();
                let trimmed = inner.trim();
                if classify_inner(&inner, depth + 1) != CommandSafety::AlwaysSafe {
                    return None;
                }
                output.push_str(SAFE_SUBSTITUTION_PLACEHOLDER);
                // `$(pwd)` is deterministic and heavily used by build env
                // assignments; anything else keeps the outer command in
                // judge territory.
                if trimmed != "pwd" {
                    requires_judgment = true;
                }
                index = close + 1;
                continue;
            }
        }
        output.push(value);
        index += 1;
    }
    (quote.is_none() && !escaped).then_some(Expansion {
        command: output,
        requires_judgment,
    })
}

fn closing_parenthesis(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (index, value) in chars.iter().enumerate().skip(open) {
        if escaped {
            escaped = false;
            continue;
        }
        if *value == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if (*value == '\'' || *value == '"') && quote.is_none_or(|active| active == *value) {
            quote = if quote.is_some() { None } else { Some(*value) };
            continue;
        }
        if quote.is_some() {
            continue;
        }
        if *value == '(' {
            depth += 1;
        } else if *value == ')' {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

/// Split a command line on `|`, `||`, `&&`, `&`, `;`, and newlines, ignoring
/// separators inside quotes. Returns `None` on unbalanced quoting.
fn split_segments(command: &str) -> Option<Vec<String>> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for value in command.chars() {
        if escaped {
            current.push(value);
            escaped = false;
            continue;
        }
        if value == '\\' && quote != Some('\'') {
            escaped = true;
            current.push(value);
            continue;
        }
        if (value == '\'' || value == '"') && quote.is_none_or(|active| active == value) {
            quote = if quote.is_some() { None } else { Some(value) };
            current.push(value);
            continue;
        }
        if quote.is_none() && matches!(value, '|' | '&' | ';' | '\n' | '\r') {
            let segment = current.trim().to_owned();
            if !segment.is_empty() {
                segments.push(segment);
            }
            current.clear();
            continue;
        }
        current.push(value);
    }
    if quote.is_some() {
        return None;
    }
    let segment = current.trim().to_owned();
    if !segment.is_empty() {
        segments.push(segment);
    }
    Some(segments)
}

fn tokenize(segment: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut text = String::new();
    let mut quoted = false;
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for value in segment.chars() {
        if escaped {
            text.push(value);
            started = true;
            escaped = false;
            continue;
        }
        if value == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if (value == '\'' || value == '"') && quote.is_none_or(|active| active == value) {
            quote = if quote.is_some() { None } else { Some(value) };
            quoted = true;
            started = true;
            continue;
        }
        if quote.is_none() && value.is_whitespace() {
            if started {
                tokens.push(Token {
                    text: std::mem::take(&mut text),
                    quoted,
                });
                quoted = false;
                started = false;
            }
            continue;
        }
        text.push(value);
        started = true;
    }
    if started {
        tokens.push(Token { text, quoted });
    }
    tokens
}

fn strip_env_assignments(tokens: Vec<Token>) -> Vec<Token> {
    let leading = tokens
        .iter()
        .take_while(|token| !token.quoted && is_env_assignment(&token.text))
        .count();
    tokens.into_iter().skip(leading).collect()
}

fn strip_prefix_keywords(tokens: Vec<Token>) -> Vec<Token> {
    let leading = tokens
        .iter()
        .take_while(|token| {
            !token.quoted
                && PREFIX_CONTROL_KEYWORDS.contains(&token.text.to_ascii_lowercase().as_str())
        })
        .count();
    let remaining = tokens.into_iter().skip(leading).collect::<Vec<_>>();
    if remaining
        .first()
        .is_some_and(|token| !token.quoted && is_env_assignment(&token.text))
    {
        return strip_prefix_keywords(strip_env_assignments(remaining));
    }
    remaining
}

fn is_env_assignment(token: &str) -> bool {
    let Some((key, _)) = token.split_once('=') else {
        return false;
    };
    !key.is_empty()
        && key
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || value == '_')
        && !key
            .chars()
            .next()
            .is_some_and(|value| value.is_ascii_digit())
}

fn basename(token: &str) -> &str {
    token.rsplit('/').next().unwrap_or(token)
}

/// MCP tool names that read state. Used to skip the judge for obviously
/// read-only MCP calls, mirroring the shell allowlist.
pub fn mcp_tool_name_looks_read_only(tool: &str) -> bool {
    let lowered = tool.to_ascii_lowercase();
    const READ_PREFIXES: &[&str] = &[
        "get_",
        "list_",
        "read_",
        "search_",
        "find_",
        "fetch_",
        "query_",
        "describe_",
        "show_",
        "inspect_",
        "count_",
        "check_",
        "view_",
        "browse_",
        "lookup_",
        "resolve_",
    ];
    const WRITE_MARKERS: &[&str] = &[
        "create", "update", "delete", "remove", "write", "send", "post", "put", "patch", "exec",
        "run", "install", "deploy", "publish", "upload", "modify", "set_", "move", "rename",
    ];
    if WRITE_MARKERS.iter().any(|marker| lowered.contains(marker)) {
        return false;
    }
    READ_PREFIXES
        .iter()
        .any(|prefix| lowered.starts_with(prefix))
        || matches!(lowered.as_str(), "ls" | "cat" | "status" | "ping")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn safe(command: &str) {
        assert_eq!(
            classify(command),
            CommandSafety::AlwaysSafe,
            "expected safe: {command}"
        );
    }

    fn dangerous(command: &str) {
        assert_eq!(
            classify(command),
            CommandSafety::AlwaysDangerous,
            "expected dangerous: {command}"
        );
    }

    fn judged(command: &str) {
        assert_eq!(
            classify(command),
            CommandSafety::NeedsJudgment,
            "expected judgment: {command}"
        );
    }

    #[test]
    fn read_only_inspection_is_always_safe() {
        safe("ls -la");
        safe("cat Cargo.toml");
        safe("rg --files-with-matches approval crates");
        safe("git status --short");
        safe("git log --oneline -20");
        safe("cargo test -p willdeep-core");
        safe("cargo clippy --all-targets");
        safe("pwd");
        safe("wc -l crates/willdeep-core/src/tools.rs");
        safe("cd crates && ls");
        safe("grep -rn approval crates | head -40");
        safe("find . -name '*.rs' -type f");
        safe("echo hello");
        safe("uname -a && date");
    }

    #[test]
    fn destructive_shapes_are_never_auto_approved() {
        dangerous("rm -rf /");
        dangerous("sudo rm -rf build");
        dangerous("find . -name '*.tmp' | xargs rm");
        dangerous("chmod -R 777 .");
        dangerous("git push --force origin main");
        dangerous("git reset --hard HEAD~3");
        dangerous("git clean -fd");
        dangerous("mv src dst");
        dangerous("dd if=/dev/zero of=/dev/disk0");
        dangerous(":(){:|:&};:");
        dangerous("cp -r / /tmp/backup");
        dangerous("kill -9 4242");
        dangerous("");
    }

    #[test]
    fn quoted_dangerous_words_stay_data() {
        safe("grep -rn 'rm -rf' logs");
        safe("echo \"sudo is not used here\"");
    }

    #[test]
    fn unknown_and_effectful_commands_go_to_the_judge() {
        judged("curl https://example.com/api");
        judged("npm install");
        judged("cargo publish");
        judged("./scripts/deploy.sh");
        judged("python3 tool.py --write");
        judged("sed -i '' 's/a/b/' file.txt");
        judged("git commit -m 'wip'");
        judged("echo hi > out.txt");
        judged("ssh host 'ls'");
        judged("tar -xzf bundle.tar.gz");
    }

    #[test]
    fn harmless_redirections_do_not_force_review() {
        safe("cargo test 2>&1 | tail -20");
        safe("ls /nope 2>/dev/null");
    }

    #[test]
    fn command_substitution_is_bounded() {
        // Safe payload, deterministic: stays safe.
        safe("cd $(pwd)");
        // Safe payload, non-deterministic: whole command needs review.
        judged("echo $(git rev-parse HEAD)");
        // Unknown payload: never auto-approved.
        dangerous("echo $(curl https://example.com/x.sh)");
        dangerous("echo `whoami`");
    }

    #[test]
    fn one_unsafe_segment_poisons_the_chain() {
        dangerous("ls && rm -rf build");
        judged("ls && curl https://example.com");
    }

    #[test]
    fn workspace_create_requires_write_allowance() {
        assert_eq!(
            classify_with_workspace_write("mkdir -p build", true),
            CommandSafety::AlwaysSafe
        );
        assert_eq!(
            classify_with_workspace_write("mkdir -p build", false),
            CommandSafety::NeedsJudgment
        );
    }

    #[test]
    fn heredocs_and_unbalanced_quotes_are_not_auto_approved() {
        judged("cat <<'EOF' > file\nhello\nEOF");
        assert_ne!(classify("echo 'unterminated"), CommandSafety::AlwaysSafe);
    }

    #[test]
    fn mcp_read_only_names_are_recognized() {
        assert!(mcp_tool_name_looks_read_only("list_issues"));
        assert!(mcp_tool_name_looks_read_only("get_page_text"));
        assert!(!mcp_tool_name_looks_read_only("create_issue"));
        assert!(!mcp_tool_name_looks_read_only("get_and_delete_page"));
    }
}
