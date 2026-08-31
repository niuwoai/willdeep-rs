//! `willdeep plugin`：安装、审批、启停与卸载。
//!
//! 安装目录 `~/.willdeep/plugins/<plugin-id>/<version>/` 与 macOS 版共享，
//! 所以这里装上的插件 Xedit 也看得见，反过来也一样。共享的只有包内容——
//! 启用与审批各端各管各的。
//!
//! 安装器**不执行**包里的任何东西：没有 postinstall、没有 npm/yarn install、
//! 没有构建步骤。要跑构建请在源目录跑完再装。一个"安装时会执行代码"的
//! 插件系统，安装前的权限预览就是一句空话。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use willdeep_core::plugin::{
    ApprovalGap, PluginHost, PluginSource, installed_versions, load_package,
};

#[derive(Clone, Debug, Subcommand)]
pub enum PluginAction {
    /// List installed plugins with their approval and enablement state.
    List {
        /// Emit one stable JSON report.
        #[arg(long)]
        json: bool,
    },
    /// Show one plugin's contributions, permissions and installed versions.
    Info { id: String },
    /// Install a plugin package directory into the shared plugin directory.
    Install {
        /// Directory holding .codex-plugin/plugin.json.
        path: PathBuf,
        /// Approve the declared permissions and enable it immediately.
        #[arg(long)]
        enable: bool,
    },
    /// Discover and install the first-party plugins bundled inside an installed
    /// WillDeep/Xedit macOS app, or a checkout's PluginExamples directory.
    Import {
        /// App bundle, PluginExamples directory, or any directory of packages.
        #[arg(value_name = "PATH")]
        from: Option<PathBuf>,
        /// Approve and enable everything that imports cleanly.
        #[arg(long)]
        enable: bool,
    },
    /// Record approval for a plugin's current version, digest and permissions.
    Approve { id: String },
    /// Enable an approved plugin.
    Enable { id: String },
    /// Disable a plugin without removing it.
    Disable { id: String },
    /// Remove every installed version of a plugin and forget its approval.
    Remove {
        id: String,
        /// Required: removing a plugin deletes its installed files.
        #[arg(long)]
        yes: bool,
    },
}

pub async fn run(action: PluginAction, home: &Path) -> Result<()> {
    match action {
        PluginAction::List { json } => list(home, json).await,
        PluginAction::Info { id } => info(home, &id).await,
        PluginAction::Install { path, enable } => install(home, &path, enable).await,
        PluginAction::Import { from, enable } => import(home, from.as_deref(), enable).await,
        PluginAction::Approve { id } => approve(home, &id).await,
        PluginAction::Enable { id } => set_enabled(home, &id, true).await,
        PluginAction::Disable { id } => set_enabled(home, &id, false).await,
        PluginAction::Remove { id, yes } => remove(home, &id, yes).await,
    }
}

fn describe_gap(gap: &ApprovalGap) -> String {
    match gap {
        ApprovalGap::NeverApproved => "never approved".to_owned(),
        ApprovalGap::VersionChanged { approved } => format!("approved version was {approved}"),
        ApprovalGap::DigestChanged => "package content changed since approval".to_owned(),
        ApprovalGap::SourceChanged { approved } => format!("approved source was {approved}"),
        ApprovalGap::NewPermissions(added) => format!("new permissions: {}", added.join(", ")),
    }
}

async fn list(home: &Path, json: bool) -> Result<()> {
    let host = PluginHost::discover(home)?;
    if json {
        let mut items = Vec::new();
        for package in host.packages() {
            items.push(serde_json::json!({
                "id": package.id,
                "name": package.display_name(),
                "version": package.version,
                "source": package.source.as_str(),
                "enabled": host.is_enabled(&package.id).await,
                "approval_gap": host.approval_gap(&package.id).await?.as_ref().map(describe_gap),
                "destinations": package.manifest.as_ref().map(|manifest| manifest.destinations.len()).unwrap_or(0),
                "digest": package.digest().unwrap_or_default(),
            }));
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "plugins": items,
                "failures": host.failures().iter().map(|failure| serde_json::json!({
                    "path": failure.path.display().to_string(),
                    "reason": failure.reason,
                })).collect::<Vec<_>>(),
            }))?
        );
        return Ok(());
    }
    if host.packages().is_empty() {
        println!("No plugins installed.");
        println!(
            "Install one with: willdeep plugin install <directory>, or import the \
             first-party set with: willdeep plugin import"
        );
    }
    for package in host.packages() {
        let enabled = host.is_enabled(&package.id).await;
        let gap = host.approval_gap(&package.id).await?;
        let state = match (enabled, &gap) {
            (true, _) => "enabled".to_owned(),
            (false, Some(gap)) => format!("blocked — {}", describe_gap(gap)),
            (false, None) => "approved, disabled".to_owned(),
        };
        println!(
            "{:<34} {:<10} {:<12} {}",
            package.id,
            package.version,
            package.source.as_str(),
            state
        );
    }
    for failure in host.failures() {
        eprintln!("warning: {} — {}", failure.path.display(), failure.reason);
    }
    Ok(())
}

async fn info(home: &Path, id: &str) -> Result<()> {
    let host = PluginHost::discover(home)?;
    let package = host.package(id)?;
    println!("{} {}", package.display_name(), package.version);
    println!("id:       {}", package.id);
    println!("source:   {}", package.source.as_str());
    println!("root:     {}", package.root.display());
    println!("digest:   {}", package.digest()?);
    if let Some(description) = &package.codex.description {
        println!("about:    {description}");
    }
    match host.approval_gap(id).await? {
        None => println!("approval: current"),
        Some(gap) => println!("approval: {}", describe_gap(&gap)),
    }
    println!("enabled:  {}", host.is_enabled(id).await);

    let versions = installed_versions(&PluginHost::shared_root(home), id);
    if !versions.is_empty() {
        println!("versions: {}", versions.join(", "));
    }
    if !package.mcp_servers.is_empty() {
        println!(
            "mcp:      {}",
            package
                .mcp_servers
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let Some(manifest) = &package.manifest else {
        println!("(Codex-only package: provides skills and MCP, contributes no destination.)");
        return Ok(());
    };
    if manifest.permissions.is_empty() {
        println!("permissions: none");
    } else {
        println!(
            "permissions: {}",
            manifest
                .permissions
                .iter()
                .map(|item| item.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    for destination in &manifest.destinations {
        println!(
            "destination: {} ({})",
            package.localized(&destination.title_key, "en"),
            destination.id
        );
    }
    for command in &manifest.commands {
        println!(
            "command:     {} — {}",
            command.id,
            package.localized(&command.title_key, "en")
        );
    }
    Ok(())
}

/// 复制一个包目录到共享安装目录。
///
/// 跳过 `.git`、`node_modules` 与 `ui/.cache`：Git 元数据不进安装版本，
/// 装机产物也不是插件的一部分（把它们复制过去能让一个 8 MiB 的插件变成 2 GiB）。
fn copy_package(source: &Path, destination: &Path) -> Result<()> {
    std::fs::create_dir_all(destination)
        .with_context(|| format!("create {}", destination.display()))?;
    for entry in std::fs::read_dir(source).with_context(|| format!("read {}", source.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if matches!(name_str.as_ref(), ".git" | "node_modules" | ".cache") {
            continue;
        }
        let from = entry.path();
        let to = destination.join(&name);
        // 符号链接一律不跟：包外的东西不属于这个包。
        let metadata = std::fs::symlink_metadata(&from)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            copy_package(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)
                .with_context(|| format!("copy {} to {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

async fn install(home: &Path, path: &Path, enable: bool) -> Result<()> {
    let installed = install_one(home, path)?;
    println!("Installed {} {}", installed.0, installed.1);
    if enable {
        approve(home, &installed.0).await?;
        set_enabled(home, &installed.0, true).await?;
    } else {
        println!(
            "Review its permissions and then run: willdeep plugin approve {} && \
             willdeep plugin enable {}",
            installed.0, installed.0
        );
    }
    Ok(())
}

/// 返回 (plugin_id, version)。
fn install_one(home: &Path, path: &Path) -> Result<(String, String)> {
    let source = path
        .canonicalize()
        .with_context(|| format!("resolve {}", path.display()))?;
    let package = load_package(&source, PluginSource::Shared)
        .with_context(|| format!("load plugin package at {}", source.display()))?;
    let target = PluginHost::shared_root(home)
        .join(&package.id)
        .join(&package.version);

    // 安装这条路真的需要读包内容——不可变版本安装靠的就是指纹比对。
    let incoming = package.digest()?;
    if target.exists() {
        // 不可变版本安装：同版本内容不同一律拒绝，而不是覆盖。
        // 覆盖会让一份已经批准过的 digest 在用户不知情时换掉。
        let existing = load_package(&target, PluginSource::Shared)
            .with_context(|| format!("read installed copy at {}", target.display()))?
            .digest()?;
        if existing == incoming {
            println!("{} {} is already installed.", package.id, package.version);
            return Ok((package.id, package.version));
        }
        bail!(
            "{} {} is already installed with different content.\n  installed: {}\n  incoming:  {}\n\
             Bump the version in .codex-plugin/plugin.json, or remove the plugin first.",
            package.id,
            package.version,
            existing,
            incoming
        );
    }

    // 先装到临时目录再改名：一个复制到一半的包不该变成"已安装的版本"。
    let staging = PluginHost::shared_root(home)
        .join(&package.id)
        .join(format!(".staging-{}", package.version));
    let _ = std::fs::remove_dir_all(&staging);
    copy_package(&source, &staging)?;
    // 复制后重新校验：确认落盘的那一份确实还是刚才检查过的那一份。
    let staged = load_package(&staging, PluginSource::Shared).inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&staging);
    })?;
    let staged_digest = staged.digest().inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&staging);
    })?;
    if staged_digest != incoming {
        let _ = std::fs::remove_dir_all(&staging);
        bail!(
            "package changed while copying ({} became {})",
            incoming,
            staged_digest
        );
    }
    std::fs::rename(&staging, &target)
        .with_context(|| format!("move staged package into {}", target.display()))?;
    Ok((package.id, package.version))
}

/// 常见的第一方插件来源。找不到就报清楚，不猜。
fn import_candidates(from: Option<&Path>) -> Vec<PathBuf> {
    if let Some(path) = from {
        return vec![path.to_path_buf()];
    }
    let mut roots = Vec::new();
    for app in [
        "/Applications/WillDeep.app",
        "/Applications/Xedit.app",
        "/Applications/WillDeep.app/Contents/Resources",
    ] {
        roots.push(PathBuf::from(app).join("Contents/Resources/BundledPlugins"));
        roots.push(PathBuf::from(app).join("BundledPlugins"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        roots.push(home.join("Sites/Xedit/PluginExamples"));
        roots.push(home.join("Applications/WillDeep.app/Contents/Resources/BundledPlugins"));
    }
    roots
}

async fn import(home: &Path, from: Option<&Path>, enable: bool) -> Result<()> {
    let mut imported = Vec::new();
    let mut searched = Vec::new();
    for root in import_candidates(from) {
        if !root.is_dir() {
            searched.push(root);
            continue;
        }
        println!("Importing from {}", root.display());
        let entries = std::fs::read_dir(&root)?;
        let mut directories: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.join(".codex-plugin").join("plugin.json").is_file())
            .collect();
        directories.sort();
        for directory in directories {
            match install_one(home, &directory) {
                Ok((id, version)) => {
                    println!("  {id} {version}");
                    imported.push(id);
                }
                Err(error) => eprintln!("  skipped {}: {error}", directory.display()),
            }
        }
        break;
    }
    if imported.is_empty() {
        bail!(
            "no plugin packages found. Looked in:\n  {}\nPass a directory explicitly: \
             willdeep plugin import <path>",
            searched
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join("\n  ")
        );
    }
    if enable {
        for id in &imported {
            approve(home, id).await?;
            set_enabled(home, id, true).await?;
        }
    } else {
        println!(
            "\nImported {} plugin(s). Approve and enable them with:\n  willdeep plugin approve <id> \
             && willdeep plugin enable <id>",
            imported.len()
        );
    }
    Ok(())
}

async fn approve(home: &Path, id: &str) -> Result<()> {
    let host = PluginHost::discover(home)?;
    let package = host.package(id)?;
    let permissions = package
        .manifest
        .as_ref()
        .map(|manifest| {
            manifest
                .permissions
                .iter()
                .map(|item| item.as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    println!("Approving {} {}", package.id, package.version);
    println!("  digest:      {}", package.digest()?);
    println!("  source:      {}", package.source.as_str());
    println!(
        "  permissions: {}",
        if permissions.is_empty() {
            "none".to_owned()
        } else {
            permissions.join(", ")
        }
    );
    if !package.mcp_servers.is_empty() {
        println!(
            "  runs:        {}",
            package
                .mcp_servers
                .values()
                .map(|spec| spec.command.clone())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default();
    host.approve(id, now).await?;
    println!("Approved.");
    Ok(())
}

async fn set_enabled(home: &Path, id: &str, enabled: bool) -> Result<()> {
    let host = PluginHost::discover(home)?;
    match host.set_enabled(id, enabled).await? {
        Ok(()) => {
            println!(
                "{id} is now {}.",
                if enabled { "enabled" } else { "disabled" }
            );
            Ok(())
        }
        Err(gap) => bail!(
            "cannot enable {id}: {}.\nRun: willdeep plugin approve {id}",
            describe_gap(&gap)
        ),
    }
}

async fn remove(home: &Path, id: &str, yes: bool) -> Result<()> {
    let host = PluginHost::discover(home)?;
    let package = host.package(id)?;
    if package.source != PluginSource::Shared {
        bail!(
            "{id} comes from {} and is not managed here",
            package.source.as_str()
        );
    }
    let root = PluginHost::shared_root(home).join(id);
    if !yes {
        let versions = installed_versions(&PluginHost::shared_root(home), id);
        bail!(
            "removing {id} deletes {} and every installed version ({}). \
             Re-run with --yes to confirm.",
            root.display(),
            versions.join(", ")
        );
    }
    let canonical = root
        .canonicalize()
        .with_context(|| format!("resolve {}", root.display()))?;
    let shared = PluginHost::shared_root(home).canonicalize()?;
    if !canonical.starts_with(&shared) || canonical == shared {
        bail!("refusing to remove a path outside {}", shared.display());
    }
    std::fs::remove_dir_all(&canonical)
        .with_context(|| format!("remove {}", canonical.display()))?;
    host.forget(id).await?;
    println!("Removed {id}.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("willdeep-plugin-cmd-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("scratch");
        root
    }

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent");
        }
        std::fs::write(path, contents).expect("write");
    }

    fn source_package(root: &Path, version: &str, body: &str) {
        write(
            &root.join(".codex-plugin/plugin.json"),
            &format!(r#"{{"name":"demo","version":"{version}"}}"#),
        );
        write(
            &root.join(".willdeep-plugin/plugin.json"),
            r#"{"schemaVersion":1,"contributes":{
                "pages":[{"id":"p","runtime":"localWeb","entryPath":"ui/index.html"}]}}"#,
        );
        write(&root.join("ui/index.html"), body);
    }

    #[test]
    fn installs_into_the_shared_directory_and_is_idempotent() {
        let home = scratch("install");
        let source = home.join("src");
        source_package(&source, "1.0.0", "<html>one</html>");
        let (id, version) = install_one(&home, &source).expect("installs");
        assert_eq!((id.as_str(), version.as_str()), ("demo", "1.0.0"));
        assert!(home.join("plugins/demo/1.0.0/ui/index.html").is_file());
        // 同内容重装是空操作，不是错误。
        install_one(&home, &source).expect("re-install is a no-op");
    }

    #[test]
    fn refuses_to_overwrite_a_version_whose_content_changed() {
        let home = scratch("immutable");
        let source = home.join("src");
        source_package(&source, "1.0.0", "<html>one</html>");
        install_one(&home, &source).expect("installs");
        // 同版本换内容：覆盖会让一份已批准的 digest 在用户不知情时被换掉。
        source_package(&source, "1.0.0", "<html>two</html>");
        let error = install_one(&home, &source).expect_err("must refuse");
        assert!(error.to_string().contains("different content"));
    }

    #[test]
    fn does_not_copy_git_metadata_or_installed_dependencies() {
        let home = scratch("skip");
        let source = home.join("src");
        source_package(&source, "1.0.0", "<html>one</html>");
        write(&source.join(".git/config"), "[core]");
        write(
            &source.join("node_modules/left-pad/index.js"),
            "module.exports=1",
        );
        install_one(&home, &source).expect("installs");
        assert!(!home.join("plugins/demo/1.0.0/.git").exists());
        assert!(!home.join("plugins/demo/1.0.0/node_modules").exists());
    }

    #[test]
    fn a_failed_copy_leaves_no_half_installed_version() {
        let home = scratch("staging");
        let source = home.join("src");
        // 缺 entryPath 指向的文件仍然能装（清单不校验文件存在），
        // 这里改成缺 Codex 清单，让加载在最早一步就失败。
        write(&source.join("ui/index.html"), "<html></html>");
        assert!(install_one(&home, &source).is_err());
        assert!(!home.join("plugins/demo").exists());
    }
}
