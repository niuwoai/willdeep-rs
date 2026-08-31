//! 插件包：发现、路径安全、本地化与内容 digest。
//!
//! 安装版本位于 `~/.willdeep/plugins/<plugin-id>/<version>/`，这个目录与 macOS 版
//! WillDeep（Xedit）共享——Xedit 装过的插件这里直接看得见。共享的只有**包内容**：
//! 启用状态与权限审批各端各存各的（见 `registry.rs`），因为 Web 宿主的沙箱边界
//! 和原生宿主不是一回事，跨宿主复用审批等于偷改另一侧的安全策略。

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;

use sha2::{Digest, Sha256};

use super::manifest::{CodexManifest, ManifestError, PluginManifest};

/// 包体积上限，与 Swift 侧逐项对齐。超限不是"截断后继续"，是拒装：
/// 一个 300 MiB 的插件包多半不是插件，是有人把 node_modules 打了进来。
pub const MAX_PACKAGE_FILES: usize = 20_000;
pub const MAX_PACKAGE_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
/// 清单与动态声明式 UI 文档。
pub const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
/// Web / MCP App 页面资源。
pub const MAX_PAGE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, thiserror::Error)]
pub enum PackageError {
    #[error("plugin package `{0}` has no .codex-plugin/plugin.json")]
    MissingCodexManifest(String),
    #[error("cannot read `{path}`: {reason}")]
    Unreadable { path: String, reason: String },
    #[error("plugin manifest error: {0}")]
    Manifest(#[from] ManifestError),
    #[error("resource path `{0}` escapes the plugin package")]
    PathEscape(String),
    #[error("resource `{path}` is {size} bytes, over the {limit} byte limit")]
    TooLarge { path: String, size: u64, limit: u64 },
    #[error("plugin package has {0} files, over the {MAX_PACKAGE_FILES} file limit")]
    TooManyFiles(usize),
    #[error("plugin package is {0} bytes, over the {MAX_PACKAGE_BYTES} byte limit")]
    PackageTooLarge(u64),
    #[error("plugin `{id}` requires WillDeep {required}, this build is {current}")]
    VersionTooOld {
        id: String,
        required: String,
        current: String,
    },
    #[error("plugin `{server}` declares a plaintext credential in mcp.json under `{key}`")]
    PlaintextCredential { server: String, key: String },
}

/// 插件包的来源。审批记录绑定来源：来源变了就要重新确认，
/// 因为"同一个 ID 同一个版本"从别处来的时候，它不是同一个东西。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginSource {
    /// `~/.willdeep/plugins/`：与 Xedit 共享的安装目录。
    Shared,
    /// `~/.codex/plugins/cache`：只读发现。
    CodexCache,
    /// `~/.willdeep/plugin-drafts`：AI 草案，永远需要人工批准。
    Draft,
}

impl PluginSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::CodexCache => "codex-cache",
            Self::Draft => "draft",
        }
    }
}

/// 一个已解析的插件包：身份、贡献点、本地化与内容 digest。
#[derive(Clone, Debug)]
pub struct PluginPackage {
    pub id: String,
    pub version: String,
    pub root: PathBuf,
    pub source: PluginSource,
    pub codex: CodexManifest,
    /// 只有 Codex 清单的包仍可提供 Skill/MCP，但不贡献一级入口。
    pub manifest: Option<PluginManifest>,
    pub locales: BTreeMap<String, BTreeMap<String, String>>,
    pub mcp_servers: BTreeMap<String, McpServerSpec>,
}

/// 插件自带的 MCP 服务定义（包内 `mcp.json`）。
#[derive(Clone, Debug)]
pub struct McpServerSpec {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub startup_timeout_seconds: u64,
}

impl PluginPackage {
    /// 按 locale 取一条文案，回落顺序：请求的语言 → 语言主标签 → 英文 → 键本身。
    /// 回落到键本身而不是空串，是为了让缺失在界面上肉眼可见而不是静默消失。
    pub fn localized(&self, key: &str, locale: &str) -> String {
        let base = locale.split(['-', '_']).next().unwrap_or(locale);
        let lookup = |name: &str| {
            self.locales
                .get(name)
                .and_then(|table| table.get(key))
                .filter(|value| !value.trim().is_empty())
                .cloned()
        };
        if let Some(value) = lookup(locale).or_else(|| lookup(base)) {
            return value;
        }
        // 插件的中文文件叫 `zh-Hans`（Xedit 的写法），浏览器报的却常是 `zh` 或
        // `zh-CN`。前缀匹配必须排在英文回落**之前**，否则中文界面会整片说英文。
        for (name, table) in &self.locales {
            if name.split(['-', '_']).next() == Some(base)
                && let Some(value) = table.get(key).filter(|value| !value.trim().is_empty())
            {
                return value.clone();
            }
        }
        lookup("en").unwrap_or_else(|| key.to_owned())
    }

    pub fn display_name(&self) -> String {
        self.codex
            .display_name
            .clone()
            .unwrap_or_else(|| self.id.clone())
    }

    /// 这个包的内容指纹。
    ///
    /// 按需计算：算一次要读遍包内容，而只有比对审批时才真的需要它。结果按
    /// (文件名, 大小, mtime) 在进程内缓存，所以常驻进程里重复调用是便宜的。
    pub fn digest(&self) -> Result<String, PackageError> {
        package_digest(&self.root)
    }

    /// 包内相对路径 → 绝对路径，逐项做规范化、符号链接解析与包根前缀校验。
    pub fn resource_path(&self, relative: &str) -> Result<PathBuf, PackageError> {
        safe_resource_path(&self.root, relative)
    }

    pub fn read_resource(&self, relative: &str, limit: u64) -> Result<Vec<u8>, PackageError> {
        let path = self.resource_path(relative)?;
        let metadata = fs::metadata(&path).map_err(|error| PackageError::Unreadable {
            path: relative.to_owned(),
            reason: error.to_string(),
        })?;
        if metadata.len() > limit {
            return Err(PackageError::TooLarge {
                path: relative.to_owned(),
                size: metadata.len(),
                limit,
            });
        }
        fs::read(&path).map_err(|error| PackageError::Unreadable {
            path: relative.to_owned(),
            reason: error.to_string(),
        })
    }

    pub fn read_resource_text(&self, relative: &str, limit: u64) -> Result<String, PackageError> {
        let bytes = self.read_resource(relative, limit)?;
        String::from_utf8(bytes).map_err(|_| PackageError::Unreadable {
            path: relative.to_owned(),
            reason: "resource is not valid UTF-8".to_owned(),
        })
    }

    /// 最低宿主版本校验。语义与 Swift 侧一致：只比较正式版本三段，
    /// rc 后缀不参与——插件作者写 `1.300.0-rc5` 时想表达的是"1.300.0 以后"。
    pub fn check_minimum_version(&self, current: &str) -> Result<(), PackageError> {
        let Some(manifest) = &self.manifest else {
            return Ok(());
        };
        let Some(required) = &manifest.minimum_willdeep_version else {
            return Ok(());
        };
        if release_triple(current) >= release_triple(required) {
            return Ok(());
        }
        Err(PackageError::VersionTooOld {
            id: self.id.clone(),
            required: required.clone(),
            current: current.to_owned(),
        })
    }
}

fn release_triple(value: &str) -> (u64, u64, u64) {
    let core = value.split('-').next().unwrap_or(value);
    let mut parts = core.split('.').map(|item| item.parse::<u64>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

/// 把一个包内相对路径解析成绝对路径，拒绝一切逃逸。
///
/// 三道关缺一不可：先在**词法上**拒掉 `..` 与绝对路径，再对存在的父目录做
/// 真实符号链接解析，最后校验解析结果仍在包根之下。只做最后一步是不够的——
/// 中间某一层是符号链接时，词法拼接出来的路径看着很乖。
pub fn safe_resource_path(root: &Path, relative: &str) -> Result<PathBuf, PackageError> {
    let candidate = Path::new(relative);
    if candidate.is_absolute() {
        return Err(PackageError::PathEscape(relative.to_owned()));
    }
    let mut normalized = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(PackageError::PathEscape(relative.to_owned()));
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(PackageError::PathEscape(relative.to_owned()));
    }
    let root = root
        .canonicalize()
        .map_err(|error| PackageError::Unreadable {
            path: root.display().to_string(),
            reason: error.to_string(),
        })?;
    let joined = root.join(&normalized);
    let resolved = joined
        .canonicalize()
        .map_err(|error| PackageError::Unreadable {
            path: relative.to_owned(),
            reason: error.to_string(),
        })?;
    if !resolved.starts_with(&root) {
        return Err(PackageError::PathEscape(relative.to_owned()));
    }
    Ok(resolved)
}

/// 扫描并解析一个插件包目录。
pub fn load_package(root: &Path, source: PluginSource) -> Result<PluginPackage, PackageError> {
    let codex_path = root.join(".codex-plugin").join("plugin.json");
    let codex_source = read_text(&codex_path, MAX_MANIFEST_BYTES)
        .map_err(|_| PackageError::MissingCodexManifest(root.display().to_string()))?;
    let codex = CodexManifest::parse(&codex_source)?;

    let willdeep_path = root.join(".willdeep-plugin").join("plugin.json");
    let manifest = if willdeep_path.is_file() {
        Some(PluginManifest::parse(&read_text(
            &willdeep_path,
            MAX_MANIFEST_BYTES,
        )?)?)
    } else {
        None
    };

    let locales = load_locales(root)?;
    if let Some(manifest) = &manifest {
        let english = locales.get("en").cloned().unwrap_or_default();
        manifest.validate_localization(&english)?;
    }

    let mcp_servers = load_mcp_servers(root)?;

    // 这里刻意**不**算 digest：算它要把包内容整个读一遍，而发现阶段只需要
    // 知道「装了什么」。本机 30 MB 的插件目录，光是列个清单就为此花掉数秒，
    // 而其中最大的那个包压根没被批准过、根本轮不到比对指纹。
    Ok(PluginPackage {
        id: codex.id.clone(),
        version: codex.version.clone(),
        root: root.to_path_buf(),
        source,
        codex,
        manifest,
        locales,
        mcp_servers,
    })
}

fn read_text(path: &Path, limit: u64) -> Result<String, PackageError> {
    let metadata = fs::metadata(path).map_err(|error| PackageError::Unreadable {
        path: path.display().to_string(),
        reason: error.to_string(),
    })?;
    if metadata.len() > limit {
        return Err(PackageError::TooLarge {
            path: path.display().to_string(),
            size: metadata.len(),
            limit,
        });
    }
    fs::read_to_string(path).map_err(|error| PackageError::Unreadable {
        path: path.display().to_string(),
        reason: error.to_string(),
    })
}

fn load_locales(root: &Path) -> Result<BTreeMap<String, BTreeMap<String, String>>, PackageError> {
    let mut locales = BTreeMap::new();
    let directory = root.join(".willdeep-plugin").join("locales");
    let Ok(entries) = fs::read_dir(&directory) else {
        return Ok(locales);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|item| item.to_str()) != Some("json") {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|item| item.to_str()) else {
            continue;
        };
        let source = read_text(&path, MAX_MANIFEST_BYTES)?;
        let parsed: BTreeMap<String, String> =
            serde_json::from_str(&source).map_err(|error| PackageError::Unreadable {
                path: path.display().to_string(),
                reason: error.to_string(),
            })?;
        locales.insert(name.to_owned(), parsed);
    }
    Ok(locales)
}

/// 明文凭据检测：`mcp.json` 的敏感 env 只能是 Keychain 引用或 `${setting:<id>}`。
/// 看见明文就拒载该服务——插件包会被复制、会被打包分发，一个写死的 token
/// 早晚会躺在某个人的下载目录里。
fn is_plaintext_credential(key: &str, value: &str) -> bool {
    let lowered = key.to_ascii_lowercase();
    let sensitive = [
        "token",
        "password",
        "secret",
        "api_key",
        "apikey",
        "credential",
    ];
    if !sensitive.iter().any(|needle| lowered.contains(needle)) {
        return false;
    }
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return false;
    }
    !(trimmed.starts_with("${setting:") || trimmed.starts_with("${keychain:"))
}

fn load_mcp_servers(root: &Path) -> Result<BTreeMap<String, McpServerSpec>, PackageError> {
    let mut servers = BTreeMap::new();
    let path = root.join("mcp.json");
    if !path.is_file() {
        return Ok(servers);
    }
    let source = read_text(&path, MAX_MANIFEST_BYTES)?;
    let value: serde_json::Value =
        serde_json::from_str(&source).map_err(|error| PackageError::Unreadable {
            path: path.display().to_string(),
            reason: error.to_string(),
        })?;
    let Some(table) = value.get("mcpServers").and_then(|item| item.as_object()) else {
        return Ok(servers);
    };
    for (name, entry) in table {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let Some(command) = entry.get("command").and_then(|item| item.as_str()) else {
            continue;
        };
        let args = entry
            .get("args")
            .and_then(|item| item.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let mut env = BTreeMap::new();
        if let Some(table) = entry.get("env").and_then(|item| item.as_object()) {
            for (key, value) in table {
                let Some(value) = value.as_str() else {
                    continue;
                };
                if is_plaintext_credential(key, value) {
                    return Err(PackageError::PlaintextCredential {
                        server: name.clone(),
                        key: key.clone(),
                    });
                }
                env.insert(key.clone(), value.to_owned());
            }
        }
        let startup_timeout_seconds = entry
            .get("startup_timeout_sec")
            .or_else(|| entry.get("startup_timeout_seconds"))
            .and_then(|item| item.as_u64())
            .unwrap_or(30)
            .clamp(1, 300);
        servers.insert(
            name.clone(),
            McpServerSpec {
                command: command.to_owned(),
                args,
                env,
                startup_timeout_seconds,
            },
        );
    }
    Ok(servers)
}

/// 一个包的 digest 缓存条目。指纹只由文件名、大小与 mtime 组成，
/// 算它不用读一个字节。
struct CachedDigest {
    fingerprint: u64,
    digest: String,
}

fn digest_cache() -> &'static Mutex<HashMap<PathBuf, CachedDigest>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedDigest>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 一个包内所有文件的 (相对路径, 大小, mtime) 汇总。
///
/// 这**不是**安全边界——安全边界是下面那个 SHA256。它只回答「这个包自上次
/// 起有没有动过」，所以用最便宜的哈希就够，和会话摘要缓存那边同一个手法。
fn fingerprint(files: &[FileEntry]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for file in files {
        file.relative.hash(&mut hasher);
        file.size.hash(&mut hasher);
        file.modified.hash(&mut hasher);
    }
    hasher.finish()
}

struct FileEntry {
    relative: String,
    size: u64,
    modified: u64,
}

/// 内容 digest：审批记录绑定它，同版本内容变了就要重新确认。
/// 路径与内容都进哈希，只哈希内容的话换个文件名照样能改行为。
///
/// 结果按 (文件名, 大小, mtime) 缓存。没有缓存时，光是列一次插件就要把
/// 全部包内容读一遍——本机 30 MB 的插件目录在 debug 构建下要 8 秒，
/// 而这份哈希在包没动过时每次都是同一个值。
pub fn package_digest(root: &Path) -> Result<String, PackageError> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files, &mut 0u64)?;
    files.sort_by(|left, right| left.relative.cmp(&right.relative));
    let fingerprint = fingerprint(&files);

    if let Some(cached) = digest_cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(root)
        .filter(|cached| cached.fingerprint == fingerprint)
    {
        return Ok(cached.digest.clone());
    }

    let mut hasher = Sha256::new();
    for file in &files {
        hasher.update(file.relative.as_bytes());
        hasher.update([0u8]);
        let path = root.join(&file.relative);
        let bytes = fs::read(&path).map_err(|error| PackageError::Unreadable {
            path: file.relative.clone(),
            reason: error.to_string(),
        })?;
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    let digest = format!("sha256:{:x}", hasher.finalize());
    digest_cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(
            root.to_path_buf(),
            CachedDigest {
                fingerprint,
                digest: digest.clone(),
            },
        );
    Ok(digest)
}

fn collect_files(
    root: &Path,
    directory: &Path,
    out: &mut Vec<FileEntry>,
    total: &mut u64,
) -> Result<(), PackageError> {
    let entries = fs::read_dir(directory).map_err(|error| PackageError::Unreadable {
        path: directory.display().to_string(),
        reason: error.to_string(),
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        // 符号链接一律不跟进：包外的东西不属于这个包，也不该进 digest。
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            // 装机产物不是插件的一部分，也不该把 digest 拖成分钟级。
            let name = entry.file_name();
            if matches!(
                name.to_str(),
                Some("node_modules") | Some(".git") | Some(".cache")
            ) {
                continue;
            }
            collect_files(root, &path, out, total)?;
            continue;
        }
        if metadata.len() > MAX_FILE_BYTES {
            return Err(PackageError::TooLarge {
                path: path.display().to_string(),
                size: metadata.len(),
                limit: MAX_FILE_BYTES,
            });
        }
        *total += metadata.len();
        if *total > MAX_PACKAGE_BYTES {
            return Err(PackageError::PackageTooLarge(*total));
        }
        if out.len() >= MAX_PACKAGE_FILES {
            return Err(PackageError::TooManyFiles(out.len() + 1));
        }
        if let Ok(relative) = path.strip_prefix(root) {
            out.push(FileEntry {
                relative: relative.to_string_lossy().replace('\\', "/"),
                size: metadata.len(),
                modified: metadata
                    .modified()
                    .ok()
                    .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                    .map(|value| value.as_nanos() as u64)
                    .unwrap_or_default(),
            });
        }
    }
    Ok(())
}

/// 发现一个目录下所有插件包。目录布局有两种：
/// `<root>/<plugin-id>/<version>/` （共享安装目录，多版本并存）
/// 与 `<root>/<plugin-id>/`（Codex 缓存与草案，单版本）。
pub fn discover(root: &Path, source: PluginSource) -> Vec<Result<PluginPackage, PackageError>> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return found;
    };
    let mut directories: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    directories.sort();
    for directory in directories {
        if directory
            .join(".codex-plugin")
            .join("plugin.json")
            .is_file()
        {
            found.push(load_package(&directory, source));
            continue;
        }
        // 多版本布局：只加载版本号最大的那个，旧版本留着供回滚。
        let Ok(versions) = fs::read_dir(&directory) else {
            continue;
        };
        let mut candidates: Vec<PathBuf> = versions
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.join(".codex-plugin").join("plugin.json").is_file())
            .collect();
        candidates.sort_by_key(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(release_triple)
                .unwrap_or_default()
        });
        if let Some(latest) = candidates.last() {
            found.push(load_package(latest, source));
        }
    }
    found
}

/// 列出一个插件已安装的全部版本，新的在前。
pub fn installed_versions(root: &Path, plugin_id: &str) -> Vec<String> {
    let mut versions: Vec<String> = fs::read_dir(root.join(plugin_id))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry
                .path()
                .join(".codex-plugin")
                .join("plugin.json")
                .is_file()
        })
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .collect();
    versions.sort_by_key(|value| std::cmp::Reverse(release_triple(value)));
    versions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("willdeep-plugin-test-{name}"));
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

    fn fixture(root: &Path) {
        write(
            &root.join(".codex-plugin/plugin.json"),
            r#"{"name":"demo","version":"1.0.0","interface":{"displayName":"Demo"}}"#,
        );
        write(
            &root.join(".willdeep-plugin/plugin.json"),
            r#"{"schemaVersion":1,"minimumWillDeepVersion":"0.1.0","contributes":{
                "destinations":[{"id":"demo","titleKey":"destination.demo","mainPage":"demo.main"}],
                "pages":[{"id":"demo.main","runtime":"localWeb","entryPath":"ui/index.html"}]}}"#,
        );
        write(
            &root.join(".willdeep-plugin/locales/en.json"),
            r#"{"destination.demo":"Demo"}"#,
        );
        write(
            &root.join(".willdeep-plugin/locales/zh-Hans.json"),
            r#"{"destination.demo":"演示"}"#,
        );
        write(&root.join("ui/index.html"), "<html><body>hi</body></html>");
    }

    #[test]
    fn loads_a_package_with_identity_contributions_and_locales() {
        let root = scratch("load");
        fixture(&root);
        let package = load_package(&root, PluginSource::Shared).expect("package loads");
        assert_eq!(package.id, "demo");
        assert_eq!(package.version, "1.0.0");
        assert!(package.digest().expect("digest").starts_with("sha256:"));
        assert_eq!(package.localized("destination.demo", "zh-Hans"), "演示");
        // 浏览器报 zh / zh-CN 时也要落到 zh-Hans 上，否则中文界面全是英文。
        assert_eq!(package.localized("destination.demo", "zh"), "演示");
        assert_eq!(package.localized("destination.demo", "zh-CN"), "演示");
        assert_eq!(package.localized("destination.demo", "fr"), "Demo");
        // 缺失的键回落成键本身，让漏翻在界面上看得见。
        assert_eq!(package.localized("nope", "en"), "nope");
    }

    #[test]
    fn loading_a_package_does_not_read_its_contents() {
        // 发现阶段只该看清单。这条盯着的是一次真实的回归：digest 曾经在
        // load_package 里算，于是「列一下装了什么」要把 30 MB 的插件目录
        // 整个读一遍，本机实测 8 秒。
        let root = scratch("lazy-digest");
        fixture(&root);
        // 一个大到读它必然明显变慢的文件；load_package 不该碰它。
        fs::write(root.join("ui/huge.bin"), vec![7u8; 24 * 1024 * 1024]).expect("write");

        let started = std::time::Instant::now();
        let package = load_package(&root, PluginSource::Shared).expect("package loads");
        let load_time = started.elapsed();

        let started = std::time::Instant::now();
        let digest = package.digest().expect("digest");
        let digest_time = started.elapsed();

        assert!(digest.starts_with("sha256:"));
        assert!(
            load_time < digest_time,
            "loading ({load_time:?}) should be cheaper than hashing ({digest_time:?})"
        );

        // 第二次走缓存，不再读文件。
        let started = std::time::Instant::now();
        assert_eq!(package.digest().expect("digest"), digest);
        assert!(started.elapsed() < digest_time);
    }

    #[test]
    fn digest_changes_when_content_changes() {
        let root = scratch("digest");
        fixture(&root);
        let first = package_digest(&root).expect("digest");
        write(&root.join("ui/index.html"), "<html><body>bye</body></html>");
        let second = package_digest(&root).expect("digest");
        assert_ne!(first, second);
    }

    #[test]
    fn resource_paths_cannot_escape_the_package() {
        let root = scratch("escape");
        fixture(&root);
        let package = load_package(&root, PluginSource::Shared).expect("package loads");
        assert!(package.resource_path("ui/index.html").is_ok());
        for attempt in ["../secrets", "/etc/passwd", "ui/../../etc/passwd", ""] {
            assert!(
                package.resource_path(attempt).is_err(),
                "path `{attempt}` should not resolve"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_pointing_outside_the_package_are_rejected() {
        let root = scratch("symlink");
        fixture(&root);
        let outside = scratch("symlink-target");
        write(&outside.join("secret.txt"), "hunter2");
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("ui/leak.txt"))
            .expect("symlink");
        let package = load_package(&root, PluginSource::Shared).expect("package loads");
        assert!(matches!(
            package.resource_path("ui/leak.txt"),
            Err(PackageError::PathEscape(_))
        ));
        // 符号链接也不该被算进 digest——包外内容不属于这个包。
        assert!(!package.digest().expect("digest").is_empty());
    }

    #[test]
    fn plaintext_credentials_in_mcp_json_are_rejected() {
        let root = scratch("credential");
        fixture(&root);
        write(
            &root.join("mcp.json"),
            r#"{"mcpServers":{"demo":{"command":"/usr/bin/ruby","env":{"API_TOKEN":"sk-live-1234"}}}}"#,
        );
        assert!(matches!(
            load_package(&root, PluginSource::Shared),
            Err(PackageError::PlaintextCredential { .. })
        ));
        write(
            &root.join("mcp.json"),
            r#"{"mcpServers":{"demo":{"command":"/usr/bin/ruby","env":{"API_TOKEN":"${setting:token}"}}}}"#,
        );
        let package = load_package(&root, PluginSource::Shared).expect("reference form loads");
        assert_eq!(package.mcp_servers.len(), 1);
    }

    #[test]
    fn minimum_version_ignores_release_candidate_suffixes() {
        let root = scratch("minversion");
        fixture(&root);
        let package = load_package(&root, PluginSource::Shared).expect("package loads");
        assert!(package.check_minimum_version("0.50.0-rc1").is_ok());
        assert!(package.check_minimum_version("0.1.0-rc1").is_ok());
        assert!(matches!(
            package.check_minimum_version("0.0.9"),
            Err(PackageError::VersionTooOld { .. })
        ));
    }

    #[test]
    fn discovery_prefers_the_newest_installed_version() {
        let root = scratch("discovery");
        for version in ["1.0.0", "1.10.0", "1.2.0"] {
            let package_root = root.join("demo").join(version);
            fixture(&package_root);
            write(
                &package_root.join(".codex-plugin/plugin.json"),
                &format!(r#"{{"name":"demo","version":"{version}"}}"#),
            );
        }
        let found = discover(&root, PluginSource::Shared);
        assert_eq!(found.len(), 1);
        let package = found.into_iter().next().expect("one").expect("loads");
        assert_eq!(package.version, "1.10.0");
        assert_eq!(
            installed_versions(&root, "demo"),
            vec!["1.10.0", "1.2.0", "1.0.0"]
        );
    }
}
