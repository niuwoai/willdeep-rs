//! OS 级写入围栏：macOS Seatbelt / Linux bubblewrap。
//!
//! 在此之前，"agent 只能改工作区里的东西"这句话由三样东西保证：审批闸门、
//! [`crate::safety`] 的静态规则、以及子 Agent 的写集校验。三样都在**进程内**——
//! 它们判的是「模型请求做什么」，而不是「进程实际能做什么」。一条被判成安全的
//! 命令自己 fork 出去写 `~/.ssh/authorized_keys`，上面三道闸门一道都不会响。
//!
//! 这一层补的就是那个差：写入范围交给内核裁决。
//!
//! # 它是什么，不是什么
//!
//! **是**写入围栏：进程能读、能跑、能联网，但只能往指定的几个根目录里写。
//! **不是**完整的牢笼：读取不受限（源码、`~/.aws/credentials` 都读得到），
//! 网络只在只读档关闭。想要完整隔离得上容器或 VM，别指望这一层。
//!
//! 这样切是故意的。从 `(deny default)` 起步的 profile 更"安全"，但 `cargo`、
//! `npm`、`git` 会因为读不到 dyld 缓存、`/dev`、证书库而以千奇百怪的方式挂掉，
//! 结果是所有人第一天就把沙箱关了——一个被关掉的沙箱防不住任何东西。
//!
//! # 两个后端，同一套语义
//!
//! | 平台 | 后端 | 机制 |
//! |---|---|---|
//! | macOS | `sandbox-exec`（Seatbelt） | profile 里 `(deny file-write*)` 后逐个 `subpath` 放行 |
//! | Linux | `bwrap`（bubblewrap） | 整个根 `--ro-bind`，可写根再 `--bind` 盖回去 |
//!
//! 两边的可观察语义必须一致：**能读、能跑，只能往列出的根里写；只读档另外断网。**
//! 一致性由同一批断言两边各跑一遍来保证，而不是靠这段文档。
//!
//! bubblewrap 不是每台机器都装了（`apt install bubblewrap` / `dnf install
//! bubblewrap`）。装了就用，没装就照实说没有——[`backend`] 返回 `None` 时，
//! 上层该告诉用户"这台机器上没有围栏"，而不是假装有。
//!
//! # 三档，对齐已有的工作区策略
//!
//! 不新造一个轴：[`SandboxPolicy`] 就是 `WorkspaceAccess` 三档的 OS 侧投影。
//!
//! | 工作区策略 | 沙箱档位 | 内核允许的写入 |
//! |---|---|---|
//! | `ReadOnly` | [`SandboxPolicy::ReadOnly`] | 无（外加禁网） |
//! | `Smart` / `WorkspaceWrite` | [`SandboxPolicy::WorkspaceWrite`] | 工作区 + 显式列出的根 |
//! | —（用户显式关闭） | [`SandboxPolicy::Off`] | 不加沙箱 |
//!
//! # 符号链接这个坑，这个仓库已经踩过一次
//!
//! 实弹靶场打出的第一个真问题就是它：审批过的写入目标没做规范化，工作区路径里
//! 只要有一层符号链接（macOS 的 `/tmp` → `/private/tmp`、`/var` → `/private/var`），
//! Worker 发出的正确补丁就会被判成越界写入。Seatbelt 的 `subpath` 匹配的是内核
//! 看到的真实路径，所以这里所有路径**必须**先 canonicalize，否则同一个 bug 会
//! 在内核层面重演一遍，而且这次报错会更难懂。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 写入围栏的档位。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SandboxPolicy {
    /// 不加沙箱。用户显式选择，或平台不支持。
    #[default]
    Off,
    /// 什么都不许写，且禁网。
    ReadOnly,
    /// 只许写 [`SandboxSpec::writable_roots`] 列出的根。
    WorkspaceWrite,
}

impl SandboxPolicy {
    /// 这一档是否需要真的套一层 `sandbox-exec`。
    pub fn is_enforcing(self) -> bool {
        !matches!(self, SandboxPolicy::Off)
    }
}

/// 一次沙箱执行的完整描述。
#[derive(Clone, Debug)]
pub struct SandboxSpec {
    pub policy: SandboxPolicy,
    /// 允许写入的根，全部为 canonical 路径。空列表在 `WorkspaceWrite` 档下
    /// 等价于「什么都不许写」——这不是错误，是调用方的选择。
    pub writable_roots: Vec<PathBuf>,
}

impl SandboxSpec {
    /// 把调用方给的路径规范化后建 spec。规范化不了的路径（不存在、权限不足）
    /// 直接丢弃：一条进不了 profile 的路径，好过一条写着符号链接、内核永远
    /// 匹配不上的路径——后者会以「命令莫名其妙失败」的形式出现。
    pub fn new(policy: SandboxPolicy, roots: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut writable_roots = Vec::new();
        if policy == SandboxPolicy::WorkspaceWrite {
            for root in roots {
                let Ok(canonical) = root.canonicalize() else {
                    continue;
                };
                if !writable_roots.contains(&canonical) {
                    writable_roots.push(canonical);
                }
            }
        }
        Self {
            policy,
            writable_roots,
        }
    }

    /// 生成 Seatbelt profile 文本。
    ///
    /// 基线是 `(allow default)` 再逐项收——理由见模块头：从 `(deny default)`
    /// 起步的 profile 在真实开发工具链上活不过第一天。
    pub fn seatbelt_profile(&self) -> String {
        let mut profile = String::from("(version 1)\n(allow default)\n");

        if self.policy == SandboxPolicy::ReadOnly {
            // 只读档连网也断：这一档的用途是"看看这个仓库有什么问题"，
            // 既不该改东西，也不该把读到的东西发出去。
            profile.push_str("(deny network*)\n");
        }

        profile.push_str("(deny file-write*)\n");

        // 写不了这几个字符设备，大量工具会以极难诊断的方式失败：
        // `command > /dev/null` 是最常见的一条。
        profile.push_str(
            "(allow file-write-data\n  \
             (literal \"/dev/null\")\n  \
             (literal \"/dev/zero\")\n  \
             (literal \"/dev/stdout\")\n  \
             (literal \"/dev/stderr\")\n  \
             (literal \"/dev/tty\"))\n",
        );

        for root in &self.writable_roots {
            profile.push_str(&format!(
                "(allow file-write* (subpath {}))\n",
                quote(&root.to_string_lossy())
            ));
        }

        profile
    }

    /// 套上沙箱之后的完整 argv。
    ///
    /// 返回 argv 而不是 `Command`，是因为调用方一半用 `std::process`、一半用
    /// `tokio::process`——返回哪一种都会逼另一半改写。`None` 表示这一档不需要
    /// 包（`Off`）或当前平台没有实现，两种情况调用方都按不加沙箱处理，但
    /// **得知道**是哪一种（用 [`available`] 区分）。
    pub fn command_line(&self, shell: &str, command: &str) -> Option<Vec<String>> {
        if !self.policy.is_enforcing() {
            return None;
        }
        match backend()? {
            SandboxBackend::Seatbelt => Some(self.seatbelt_command_line(shell, command)),
            SandboxBackend::Bubblewrap => {
                Some(self.bubblewrap_command_line(&bwrap_path()?, shell, command))
            }
        }
    }

    fn seatbelt_command_line(&self, shell: &str, command: &str) -> Vec<String> {
        vec![
            SANDBOX_EXEC.to_owned(),
            "-p".to_owned(),
            self.seatbelt_profile(),
            "--".to_owned(),
            shell.to_owned(),
            SHELL_COMMAND_FLAG.to_owned(),
            command.to_owned(),
        ]
    }

    /// bubblewrap 的等价形态：整个根挂成只读，再把可写根逐个 `--bind` 盖回去。
    ///
    /// bind 的顺序是有意义的——后面的覆盖前面的。可写根写在 `--ro-bind / /`
    /// 前面的话，它会被随后的只读挂载盖掉，于是"可写"根一个字也写不进去。
    fn bubblewrap_command_line(&self, bwrap: &Path, shell: &str, command: &str) -> Vec<String> {
        let mut argv = vec![
            bwrap.to_string_lossy().into_owned(),
            "--ro-bind".to_owned(),
            "/".to_owned(),
            "/".to_owned(),
            // `/dev` 必须是真设备节点：只读 bind 过来的 `/dev/null` 写不进去，
            // 而 `cmd > /dev/null` 是最常见的一条命令。
            "--dev".to_owned(),
            "/dev".to_owned(),
            // 父进程没了就跟着走，别在机器上留下无主的沙箱进程。
            "--die-with-parent".to_owned(),
        ];

        if self.policy == SandboxPolicy::ReadOnly {
            // 与 Seatbelt 的 `(deny network*)` 对齐：这一档不该把读到的东西发出去。
            argv.push("--unshare-net".to_owned());
        }

        for root in &self.writable_roots {
            let path = root.to_string_lossy().into_owned();
            argv.push("--bind".to_owned());
            argv.push(path.clone());
            argv.push(path);
        }

        argv.push("--".to_owned());
        argv.push(shell.to_owned());
        argv.push(SHELL_COMMAND_FLAG.to_owned());
        argv.push(command.to_owned());
        argv
    }
}

/// 与 `tools::platform_shell` 用的必须是同一个 flag。两处写岔了，沙箱里跑的
/// 就不是外面那条命令了——而这种偏差只会在某条依赖登录 shell 的命令上炸。
pub const SHELL_COMMAND_FLAG: &str = "-lc";

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
const BWRAP_CANDIDATES: &[&str] = &["/usr/bin/bwrap", "/bin/bwrap", "/usr/local/bin/bwrap"];

/// 可用的围栏实现。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SandboxBackend {
    /// macOS `sandbox-exec`。被标 deprecated 很多年，但一直能用，且是唯一
    /// 不需要额外授权就能用的进程级围栏。
    Seatbelt,
    /// Linux `bwrap`。需要机器上装了 bubblewrap。
    Bubblewrap,
}

/// 这台机器上能用哪个围栏。`None` 表示**没有**——调用方据此告诉用户实情，
/// 而不是让人以为自己有围栏。
///
/// 探测的是「能不能用」，不是「装没装」。这个区别是踩出来的：默认配置的
/// Docker 容器里 `bwrap` 明明在，跑起来却是
/// `Creating new namespace failed: Operation not permitted`——容器默认的
/// seccomp / capability profile 不给建命名空间。只查文件存在的话，我们会
/// 声称有围栏，然后**每一条命令**都以这句话失败。服务器跑在容器里是常态，
/// 这不是边角情况。
///
/// 探测一次，结果缓存到进程结束：它要 fork 一个进程，不该每条命令都来一遍。
pub fn backend() -> Option<SandboxBackend> {
    static DETECTED: OnceLock<Option<SandboxBackend>> = OnceLock::new();
    *DETECTED.get_or_init(detect_backend)
}

fn detect_backend() -> Option<SandboxBackend> {
    if cfg!(target_os = "macos")
        && Path::new(SANDBOX_EXEC).is_file()
        && probe(&[SANDBOX_EXEC, "-p", "(version 1)(allow default)", "--"])
    {
        return Some(SandboxBackend::Seatbelt);
    }
    if cfg!(target_os = "linux")
        && let Some(bwrap) = bwrap_path()
        && probe(&[
            &bwrap.to_string_lossy(),
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--",
        ])
    {
        return Some(SandboxBackend::Bubblewrap);
    }
    None
}

/// 拿最便宜的命令真跑一次围栏。跑得通才算数。
fn probe(prefix: &[&str]) -> bool {
    let Some((program, leading)) = prefix.split_first() else {
        return false;
    };
    std::process::Command::new(program)
        .args(leading)
        .arg("/bin/sh")
        .arg(SHELL_COMMAND_FLAG)
        .arg("exit 0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// 当前平台能不能真的执行沙箱。
pub fn available() -> bool {
    backend().is_some()
}

/// 找 `bwrap`。先看常见绝对路径，再翻 `PATH`——发行版之间装的位置不统一，
/// 只认一个路径会在半数机器上"没有围栏"。
fn bwrap_path() -> Option<PathBuf> {
    for candidate in BWRAP_CANDIDATES {
        let path = Path::new(candidate);
        if path.is_file() {
            return Some(path.to_path_buf());
        }
    }
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|directory| directory.join("bwrap"))
        .find(|candidate| candidate.is_file())
}

/// 这次失败像不像撞了沙箱。
///
/// 用来把「命令自己错了」和「命令被围栏拦了」分开。分不开的话，用户看到的是
/// 一句莫名其妙的 `Operation not permitted`，然后花二十分钟怀疑自己的代码。
pub fn looks_like_denial(output: &str) -> bool {
    const MARKERS: &[&str] = &[
        // macOS Seatbelt
        "Operation not permitted",
        "sandbox-exec: ",
        "deny file-write",
        // Linux bubblewrap：越界写入报的是「只读文件系统」，因为围栏就是
        // 把根挂成了只读。两边的措辞不一样，但用户要的答案是同一个。
        "Read-only file system",
        "bwrap: ",
    ];
    MARKERS.iter().any(|marker| output.contains(marker))
}

/// Seatbelt 的字符串字面量。反斜杠和引号必须转义，否则一个带引号的路径会把
/// profile 语法搞断——而语法断掉的 profile 会让 `sandbox-exec` 直接拒绝启动，
/// 表现为「所有命令都跑不了」。
fn quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for character in value.chars() {
        if character == '"' || character == '\\' {
            quoted.push('\\');
        }
        quoted.push(character);
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 仓库惯例：`temp_dir` + uuid 手建手删，不为测试引第三方依赖。
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("willdeep-sandbox-{tag}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).expect("scratch dir");
            // canonicalize：macOS 的 `/var` 是 `/private/var` 的符号链接，
            // 不规范化的话测试自己就会先踩进模块头写的那个坑。
            Self(root.canonicalize().expect("canonical scratch"))
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn child(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::create_dir_all(&path).expect("child dir");
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn workspace_spec(roots: Vec<PathBuf>) -> SandboxSpec {
        SandboxSpec::new(SandboxPolicy::WorkspaceWrite, roots)
    }

    #[test]
    fn read_only_allows_no_writable_root_even_if_asked() {
        let scratch = Scratch::new("ro-roots");
        let spec = SandboxSpec::new(SandboxPolicy::ReadOnly, vec![scratch.path().to_path_buf()]);

        assert!(spec.writable_roots.is_empty());
        assert!(!spec.seatbelt_profile().contains("file-write* (subpath"));
    }

    #[test]
    fn read_only_also_cuts_the_network() {
        let profile = SandboxSpec::new(SandboxPolicy::ReadOnly, vec![]).seatbelt_profile();

        assert!(profile.contains("(deny network*)"));
    }

    #[test]
    fn workspace_write_keeps_the_network() {
        let profile = workspace_spec(vec![]).seatbelt_profile();

        assert!(!profile.contains("(deny network*)"));
    }

    #[test]
    fn every_profile_denies_writes_before_allowing_any() {
        let scratch = Scratch::new("order");
        let profile = workspace_spec(vec![scratch.path().to_path_buf()]).seatbelt_profile();
        let deny = profile.find("(deny file-write*)").expect("deny rule");
        let allow = profile
            .find("(allow file-write* (subpath")
            .expect("allow rule");

        // Seatbelt 后写的规则覆盖先写的。顺序反了，工作区就写不进去。
        assert!(deny < allow);
    }

    #[test]
    fn writable_roots_are_canonical() {
        let scratch = Scratch::new("canon");
        let nested = scratch.child("ws");
        let indirect = scratch.path().join("ws").join(".").join("..").join("ws");

        let spec = workspace_spec(vec![indirect]);

        assert_eq!(spec.writable_roots, vec![nested]);
    }

    #[test]
    fn unresolvable_roots_are_dropped_not_guessed() {
        let scratch = Scratch::new("missing");
        let missing = scratch.path().join("does-not-exist");

        let spec = workspace_spec(vec![missing, scratch.path().to_path_buf()]);

        assert_eq!(spec.writable_roots, vec![scratch.path().to_path_buf()]);
    }

    #[test]
    fn duplicate_roots_appear_once() {
        let scratch = Scratch::new("dupe");
        let spec = workspace_spec(vec![
            scratch.path().to_path_buf(),
            scratch.path().to_path_buf(),
        ]);

        assert_eq!(spec.writable_roots.len(), 1);
    }

    #[test]
    fn dev_null_stays_writable() {
        assert!(
            workspace_spec(vec![])
                .seatbelt_profile()
                .contains("/dev/null")
        );
    }

    #[test]
    fn quotes_and_backslashes_in_paths_are_escaped() {
        assert_eq!(quote(r#"/tmp/a"b"#), r#""/tmp/a\"b""#);
        assert_eq!(quote(r"/tmp/a\b"), r#""/tmp/a\\b""#);
    }

    #[test]
    fn off_never_wraps() {
        let scratch = Scratch::new("off");
        let spec = SandboxSpec::new(SandboxPolicy::Off, vec![scratch.path().to_path_buf()]);

        assert!(spec.command_line("/bin/sh", "echo hi").is_none());
        assert!(!spec.policy.is_enforcing());
    }

    fn bwrap_argv(policy: SandboxPolicy, roots: Vec<PathBuf>) -> Vec<String> {
        SandboxSpec::new(policy, roots).bubblewrap_command_line(
            Path::new("/usr/bin/bwrap"),
            "/bin/sh",
            "echo hi",
        )
    }

    #[test]
    fn bwrap_binds_the_writable_root_after_the_read_only_root() {
        let scratch = Scratch::new("bwrap-order");
        let argv = bwrap_argv(
            SandboxPolicy::WorkspaceWrite,
            vec![scratch.path().to_path_buf()],
        );
        let ro = argv.iter().position(|a| a == "--ro-bind").expect("ro-bind");
        let rw = argv.iter().position(|a| a == "--bind").expect("bind");

        // 顺序反了，可写根会被随后的只读挂载盖掉，一个字也写不进去。
        assert!(ro < rw, "{argv:?}");
    }

    #[test]
    fn bwrap_binds_each_writable_root_onto_itself() {
        let scratch = Scratch::new("bwrap-bind");
        let root = scratch.path().to_path_buf();
        let argv = bwrap_argv(SandboxPolicy::WorkspaceWrite, vec![root.clone()]);
        let at = argv.iter().position(|a| a == "--bind").expect("bind");

        assert_eq!(argv[at + 1], root.to_string_lossy());
        assert_eq!(argv[at + 2], root.to_string_lossy());
    }

    #[test]
    fn bwrap_read_only_unshares_the_network() {
        assert!(bwrap_argv(SandboxPolicy::ReadOnly, vec![]).contains(&"--unshare-net".to_owned()));
    }

    #[test]
    fn bwrap_workspace_write_keeps_the_network() {
        assert!(
            !bwrap_argv(SandboxPolicy::WorkspaceWrite, vec![])
                .contains(&"--unshare-net".to_owned())
        );
    }

    #[test]
    fn bwrap_keeps_dev_writable() {
        // `cmd > /dev/null` 在只读 bind 的 /dev 上会失败。
        assert!(bwrap_argv(SandboxPolicy::WorkspaceWrite, vec![]).contains(&"--dev".to_owned()));
    }

    #[test]
    fn bwrap_puts_the_command_last_after_a_separator() {
        let argv = bwrap_argv(SandboxPolicy::WorkspaceWrite, vec![]);
        let separator = argv.iter().position(|a| a == "--").expect("separator");

        assert_eq!(
            argv[separator + 1..],
            ["/bin/sh", SHELL_COMMAND_FLAG, "echo hi"]
        );
    }

    #[test]
    fn both_backends_agree_on_the_read_only_contract() {
        // 一档的语义写在两个后端里，两边跑掉队的话没人会发现——除非有这条。
        let seatbelt = SandboxSpec::new(SandboxPolicy::ReadOnly, vec![]).seatbelt_profile();
        let bwrap = bwrap_argv(SandboxPolicy::ReadOnly, vec![]);

        assert!(seatbelt.contains("(deny network*)"));
        assert!(bwrap.contains(&"--unshare-net".to_owned()));
        assert!(!seatbelt.contains("file-write* (subpath"));
        assert!(!bwrap.contains(&"--bind".to_owned()));
    }

    #[test]
    fn denial_detection_separates_the_fence_from_the_bug() {
        // macOS 与 Linux 的措辞不同，用户要的答案相同。
        assert!(looks_like_denial("sh: line 1: Operation not permitted"));
        assert!(looks_like_denial(
            "/bin/sh: 1: cannot create /x/out.txt: Read-only file system"
        ));
        assert!(!looks_like_denial("error[E0433]: failed to resolve"));
        assert!(!looks_like_denial(
            "thread 'main' panicked at src/main.rs:3"
        ));
    }

    #[test]
    fn detection_survives_a_machine_with_no_backend() {
        // 只断言它不 panic、不撒谎：这台机器上有没有围栏，取决于这台机器。
        assert_eq!(available(), backend().is_some());
    }

    /// 真跑一遍。profile / argv 生成对不对只有内核说了算——上面所有断言加起来
    /// 也证明不了「工作区外面真的写不进去」，而那正是这一层存在的唯一理由。
    ///
    /// 两个平台跑的是**同一批断言**：Seatbelt 与 bubblewrap 的可观察语义必须
    /// 一致，而"一致"这件事只能由同一组测试两边各过一遍来保证，文档说了不算。
    /// 机器上没有可用后端时整批跳过（`available()` 为假），不伪装成通过。
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    mod enforced {
        use super::*;

        fn run(spec: &SandboxSpec, command: &str) -> std::process::Output {
            let argv = spec
                .command_line("/bin/sh", command)
                .expect("有可用后端时应当能包出沙箱命令");
            std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .output()
                .expect("沙箱后端应当能启动")
        }

        #[test]
        fn writes_inside_the_workspace_succeed() {
            if !available() {
                return;
            }
            let scratch = Scratch::new("inside");
            let spec = workspace_spec(vec![scratch.path().to_path_buf()]);

            let target = scratch.path().join("inside.txt");
            let output = run(&spec, &format!("echo ok > {}", target.display()));

            assert!(output.status.success(), "{output:?}");
            assert_eq!(std::fs::read_to_string(&target).unwrap().trim(), "ok");
        }

        #[test]
        fn writes_outside_the_workspace_are_denied_by_the_kernel() {
            if !available() {
                return;
            }
            let scratch = Scratch::new("outside");
            // 只把 ws 列为可写：它的父目录就是「外面」。
            let spec = workspace_spec(vec![scratch.child("ws")]);

            let target = scratch.path().join("outside.txt");
            let output = run(&spec, &format!("echo leaked > {}", target.display()));

            assert!(!output.status.success(), "越界写入居然成功了：{output:?}");
            assert!(!target.exists(), "沙箱外的文件被创建了");
        }

        #[test]
        fn read_only_denies_even_the_workspace() {
            if !available() {
                return;
            }
            let scratch = Scratch::new("ro");
            let spec =
                SandboxSpec::new(SandboxPolicy::ReadOnly, vec![scratch.path().to_path_buf()]);

            let target = scratch.path().join("nope.txt");
            let output = run(&spec, &format!("echo no > {}", target.display()));

            assert!(!output.status.success(), "只读档写成功了：{output:?}");
            assert!(!target.exists());
        }

        #[test]
        fn reading_still_works_under_the_fence() {
            if !available() {
                return;
            }
            let scratch = Scratch::new("read");
            let source = scratch.path().join("source.txt");
            std::fs::write(&source, "hello").expect("write");
            let spec = SandboxSpec::new(SandboxPolicy::ReadOnly, vec![]);

            let output = run(&spec, &format!("cat {}", source.display()));

            assert!(output.status.success(), "{output:?}");
            assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "hello");
        }

        #[test]
        fn a_path_with_a_symlink_in_it_still_matches() {
            if !available() {
                return;
            }
            // 审批层被这件事咬过一次：`/tmp` 是 `/private/tmp` 的符号链接，
            // 没规范化的路径永远匹配不上内核看到的真实路径。
            let scratch = Scratch::new("symlink");
            let real = scratch.child("real");
            let link = scratch.path().join("link");
            std::os::unix::fs::symlink(&real, &link).expect("symlink");

            let spec = workspace_spec(vec![link.clone()]);
            let output = run(
                &spec,
                &format!("echo ok > {}", link.join("f.txt").display()),
            );

            assert!(output.status.success(), "{output:?}");
            assert_eq!(
                std::fs::read_to_string(real.join("f.txt")).unwrap().trim(),
                "ok"
            );
        }
    }
}
