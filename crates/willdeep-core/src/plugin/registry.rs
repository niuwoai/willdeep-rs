//! 插件运行状态：启用与权限审批。
//!
//! 这份状态**不与 Xedit 共享**，只有包内容共享。理由：Web 宿主的沙箱边界与
//! macOS 原生宿主不同（这边是 opaque-origin iframe + CSP，那边是非持久化
//! WKWebView + 自定义协议），把审批当共享货币传过去，等于替另一个宿主替用户
//! 点了头。同一个包在两端各批一次，是成本，也是正确性。
//!
//! 文件权限固定 0600。看见 group/other 位就拒绝整个存储而不是"修好继续"——
//! 一个别人可写的审批记录，能让任意插件在下次启动时变成已批准。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::manifest::PluginPermission;
use super::package::PluginPackage;

const STATE_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("cannot read plugin registry at {path}: {reason}")]
    Read { path: String, reason: String },
    #[error("cannot write plugin registry at {path}: {reason}")]
    Write { path: String, reason: String },
    #[error(
        "plugin registry {path} is readable by group or others (mode {mode:o}); \
         fix it with chmod 600 before continuing"
    )]
    Permissions { path: String, mode: u32 },
    #[error("plugin registry at {path} is not valid JSON: {reason}")]
    Corrupt { path: String, reason: String },
    #[error("plugin registry at {path} is version {found}, this build writes {STATE_VERSION}")]
    UnknownVersion { path: String, found: u32 },
    #[error(transparent)]
    Package(#[from] super::package::PackageError),
}

/// 一次权限审批的记录。绑定四样东西：版本、内容 digest、来源与权限集合。
/// 任何一样变了都要重新确认——"我批准的是那个东西"必须可核对。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginApproval {
    pub version: String,
    pub digest: String,
    pub source: String,
    pub permissions: Vec<String>,
    pub approved_at: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PluginState {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<PluginApproval>,
    /// 固定入口的顺序；未固定的插件没有这个值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_order: Option<u32>,
    /// 非 secret 的插件设置。secret 不进这里（见下方 `SecretStore` 说明）。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub settings: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RegistryFile {
    version: u32,
    #[serde(default)]
    plugins: BTreeMap<String, PluginState>,
}

impl Default for RegistryFile {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            plugins: BTreeMap::new(),
        }
    }
}

/// 为什么一个插件不能启用。用于界面上直接说人话，而不是一个哑掉的开关。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApprovalGap {
    /// 从来没批准过。
    NeverApproved,
    /// 批准过别的版本。
    VersionChanged { approved: String },
    /// 同版本但内容变了——比换版本更需要警惕。
    DigestChanged,
    /// 来源变了：同一个 ID 同一个版本，从别处来的不是同一个东西。
    SourceChanged { approved: String },
    /// 新增了没批准过的权限。
    NewPermissions(Vec<String>),
}

impl ApprovalGap {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NeverApproved => "never-approved",
            Self::VersionChanged { .. } => "version-changed",
            Self::DigestChanged => "digest-changed",
            Self::SourceChanged { .. } => "source-changed",
            Self::NewPermissions(_) => "new-permissions",
        }
    }
}

pub struct PluginRegistry {
    path: PathBuf,
    file: RegistryFile,
}

impl PluginRegistry {
    /// 默认位置：`~/.willdeep/plugin-registry.web.json`。名字里的 `web` 是提醒——
    /// 这是 rs 宿主自己的记录，不是 Xedit 那份。
    pub fn default_path(home: &Path) -> PathBuf {
        home.join("plugin-registry.web.json")
    }

    pub fn load(path: &Path) -> Result<Self, RegistryError> {
        if !path.exists() {
            return Ok(Self {
                path: path.to_path_buf(),
                file: RegistryFile::default(),
            });
        }
        check_permissions(path)?;
        let source = fs::read_to_string(path).map_err(|error| RegistryError::Read {
            path: path.display().to_string(),
            reason: error.to_string(),
        })?;
        let file: RegistryFile =
            serde_json::from_str(&source).map_err(|error| RegistryError::Corrupt {
                path: path.display().to_string(),
                reason: error.to_string(),
            })?;
        // 未来版本写的文件这里读不懂。拒绝比"按 1 解释"安全：后者可能把一条
        // 新语义的审批当成旧语义放行。
        if file.version != STATE_VERSION {
            return Err(RegistryError::UnknownVersion {
                path: path.display().to_string(),
                found: file.version,
            });
        }
        Ok(Self {
            path: path.to_path_buf(),
            file,
        })
    }

    pub fn state(&self, plugin_id: &str) -> Option<&PluginState> {
        self.file.plugins.get(plugin_id)
    }

    pub fn is_enabled(&self, plugin_id: &str) -> bool {
        self.file
            .plugins
            .get(plugin_id)
            .is_some_and(|state| state.enabled)
    }

    /// 当前包与已有审批之间的差距。`None` 表示可以直接启用。
    ///
    /// 检查顺序是精心排的，也顺带避开了最贵的那一步：没批准过、版本变了、
    /// 权限多了，这三条都不必读包内容就能判定，只有走到 digest 比对时才需要
    /// 真的把包哈希一遍。从没批准过的插件（比如一个 26 MB 的 Codex 兼容包）
    /// 因此一个字节都不会被读。
    pub fn approval_gap(
        &self,
        package: &PluginPackage,
    ) -> Result<Option<ApprovalGap>, RegistryError> {
        let requested = permission_names(package);
        let Some(approval) = self
            .file
            .plugins
            .get(&package.id)
            .and_then(|state| state.approval.as_ref())
        else {
            return Ok(Some(ApprovalGap::NeverApproved));
        };
        if approval.version != package.version {
            return Ok(Some(ApprovalGap::VersionChanged {
                approved: approval.version.clone(),
            }));
        }
        // 权限差异排在 digest 之前是有意的：改权限必然改 digest，先报
        // DigestChanged 只会告诉用户"变了"，而他真正需要知道的是"它现在还想要
        // 网络访问"。理由越具体，那一次点头才越算数。
        let added: Vec<String> = requested
            .iter()
            .filter(|item| !approval.permissions.contains(item))
            .cloned()
            .collect();
        if !added.is_empty() {
            return Ok(Some(ApprovalGap::NewPermissions(added)));
        }
        if approval.source != package.source.as_str() {
            return Ok(Some(ApprovalGap::SourceChanged {
                approved: approval.source.clone(),
            }));
        }
        // 最后才读包内容。到这里为止，前面几条都只看清单与注册表。
        if approval.digest != package.digest()? {
            return Ok(Some(ApprovalGap::DigestChanged));
        }
        Ok(None)
    }

    /// 记录一次审批。调用方必须已经把权限清单摆给用户看过——这个函数只负责落盘。
    pub fn approve(&mut self, package: &PluginPackage, now: u64) -> Result<(), RegistryError> {
        // 审批要绑定内容指纹，所以这一步必须真的读一遍包。
        let digest = package.digest()?;
        let entry = self.file.plugins.entry(package.id.clone()).or_default();
        entry.approval = Some(PluginApproval {
            version: package.version.clone(),
            digest,
            source: package.source.as_str().to_owned(),
            permissions: permission_names(package),
            approved_at: now,
        });
        self.persist()
    }

    /// 启用一个插件。审批不齐时拒绝并说明差在哪，不静默放行。
    pub fn set_enabled(
        &mut self,
        package: &PluginPackage,
        enabled: bool,
    ) -> Result<Result<(), ApprovalGap>, RegistryError> {
        if enabled && let Some(gap) = self.approval_gap(package)? {
            return Ok(Err(gap));
        }
        let entry = self.file.plugins.entry(package.id.clone()).or_default();
        entry.enabled = enabled;
        self.persist()?;
        Ok(Ok(()))
    }

    pub fn set_pinned_order(
        &mut self,
        plugin_id: &str,
        order: Option<u32>,
    ) -> Result<(), RegistryError> {
        let entry = self.file.plugins.entry(plugin_id.to_owned()).or_default();
        entry.pinned_order = order;
        self.persist()
    }

    pub fn set_setting(
        &mut self,
        plugin_id: &str,
        key: &str,
        value: Option<&str>,
    ) -> Result<(), RegistryError> {
        let entry = self.file.plugins.entry(plugin_id.to_owned()).or_default();
        match value {
            Some(value) => entry.settings.insert(key.to_owned(), value.to_owned()),
            None => entry.settings.remove(key),
        };
        self.persist()
    }

    /// 卸载：移除该插件的全部授权与运行状态。包文件由调用方删除。
    pub fn forget(&mut self, plugin_id: &str) -> Result<(), RegistryError> {
        self.file.plugins.remove(plugin_id);
        self.persist()
    }

    fn persist(&self) -> Result<(), RegistryError> {
        debug_assert_eq!(self.file.version, STATE_VERSION);
        let source =
            serde_json::to_string_pretty(&self.file).map_err(|error| RegistryError::Write {
                path: self.path.display().to_string(),
                reason: error.to_string(),
            })?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|error| RegistryError::Write {
                path: parent.display().to_string(),
                reason: error.to_string(),
            })?;
        }
        // 先写临时文件再改名：审批记录写到一半被打断，剩下的应该是旧的完整状态，
        // 而不是半截 JSON——后者会在下次启动时把整个存储判为损坏。
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, source.as_bytes()).map_err(|error| RegistryError::Write {
            path: temporary.display().to_string(),
            reason: error.to_string(),
        })?;
        restrict_permissions(&temporary)?;
        fs::rename(&temporary, &self.path).map_err(|error| RegistryError::Write {
            path: self.path.display().to_string(),
            reason: error.to_string(),
        })?;
        Ok(())
    }
}

fn permission_names(package: &PluginPackage) -> Vec<String> {
    package
        .manifest
        .as_ref()
        .map(|manifest| {
            manifest
                .permissions
                .iter()
                .map(|item| item.as_str().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// 没有 WillDeep 清单的 Codex 兼容包：按 `mcp.json` 推断它至少要什么权限，
/// 好让安装预览不至于显示"本插件不需要任何权限"这种明显不实的话。
pub fn inferred_permissions(package: &PluginPackage) -> Vec<PluginPermission> {
    if package.manifest.is_some() || package.mcp_servers.is_empty() {
        return Vec::new();
    }
    vec![PluginPermission::ProcessExecute]
}

#[cfg(unix)]
fn check_permissions(path: &Path) -> Result<(), RegistryError> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::metadata(path).map_err(|error| RegistryError::Read {
        path: path.display().to_string(),
        reason: error.to_string(),
    })?;
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(RegistryError::Permissions {
            path: path.display().to_string(),
            mode,
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_permissions(_path: &Path) -> Result<(), RegistryError> {
    Ok(())
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<(), RegistryError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
        RegistryError::Write {
            path: path.display().to_string(),
            reason: error.to_string(),
        }
    })
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<(), RegistryError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::package::{PluginSource, load_package};

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("willdeep-registry-test-{name}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("scratch directory");
        root
    }

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent directory");
        }
        fs::write(path, contents).expect("write fixture");
    }

    fn package(root: &Path, permissions: &str) -> PluginPackage {
        write(
            &root.join(".codex-plugin/plugin.json"),
            r#"{"name":"demo","version":"1.0.0"}"#,
        );
        write(
            &root.join(".willdeep-plugin/plugin.json"),
            &format!(
                r#"{{"schemaVersion":1,"permissions":{permissions},"contributes":{{
                    "pages":[{{"id":"p","runtime":"localWeb","entryPath":"ui/index.html"}}]}}}}"#
            ),
        );
        write(&root.join("ui/index.html"), "<html></html>");
        load_package(root, PluginSource::Shared).expect("package loads")
    }

    #[test]
    fn enabling_requires_an_approval_that_matches_the_package() {
        let home = scratch("approve");
        let installed = package(&home.join("pkg"), r#"["process.execute"]"#);
        let path = PluginRegistry::default_path(&home);
        let mut registry = PluginRegistry::load(&path).expect("load");

        assert_eq!(
            registry.approval_gap(&installed).expect("gap"),
            Some(ApprovalGap::NeverApproved)
        );
        assert!(matches!(
            registry.set_enabled(&installed, true).expect("write"),
            Err(ApprovalGap::NeverApproved)
        ));
        assert!(!registry.is_enabled("demo"));

        registry.approve(&installed, 1).expect("approve");
        assert_eq!(registry.approval_gap(&installed).expect("gap"), None);
        assert!(
            registry
                .set_enabled(&installed, true)
                .expect("write")
                .is_ok()
        );
        assert!(registry.is_enabled("demo"));

        let reloaded = PluginRegistry::load(&path).expect("reload");
        assert!(reloaded.is_enabled("demo"));
    }

    #[test]
    fn changed_content_and_new_permissions_invalidate_an_approval() {
        let home = scratch("invalidate");
        let package_root = home.join("pkg");
        let installed = package(&package_root, r#"["process.execute"]"#);
        let mut registry =
            PluginRegistry::load(&PluginRegistry::default_path(&home)).expect("load");
        registry.approve(&installed, 1).expect("approve");

        // 同版本、同来源，只改内容：这比换版本更该拦。
        write(&package_root.join("ui/index.html"), "<html>changed</html>");
        let tampered = load_package(&package_root, PluginSource::Shared).expect("reload");
        assert_eq!(
            registry.approval_gap(&tampered).expect("gap"),
            Some(ApprovalGap::DigestChanged)
        );

        // 插件加要权限时，用户该看到的理由是"它现在还想要网络访问"，
        // 不是笼统的"内容变了"——改权限必然改 digest，两者会同时成立。
        let widened = package(&package_root, r#"["process.execute","network.access"]"#);
        assert!(matches!(
            registry.approval_gap(&widened).expect("gap"),
            Some(ApprovalGap::NewPermissions(added)) if added == vec!["network.access".to_owned()]
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_world_readable_registry_is_refused_outright() {
        use std::os::unix::fs::PermissionsExt;
        let home = scratch("permissions");
        let installed = package(&home.join("pkg"), "[]");
        let path = PluginRegistry::default_path(&home);
        let mut registry = PluginRegistry::load(&path).expect("load");
        registry.approve(&installed, 1).expect("approve");

        let mode = fs::metadata(&path).expect("metadata").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "registry must be written as 0600");

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("loosen");
        assert!(matches!(
            PluginRegistry::load(&path),
            Err(RegistryError::Permissions { .. })
        ));
    }

    #[test]
    fn codex_only_packages_are_shown_as_needing_process_execution() {
        let home = scratch("inferred");
        let root = home.join("pkg");
        write(
            &root.join(".codex-plugin/plugin.json"),
            r#"{"name":"demo","version":"1.0.0"}"#,
        );
        write(
            &root.join("mcp.json"),
            r#"{"mcpServers":{"demo":{"command":"/usr/bin/ruby","args":["x.rb"]}}}"#,
        );
        let installed = load_package(&root, PluginSource::Shared).expect("loads");
        assert!(installed.manifest.is_none());
        assert_eq!(
            inferred_permissions(&installed),
            vec![PluginPermission::ProcessExecute]
        );
    }
}
