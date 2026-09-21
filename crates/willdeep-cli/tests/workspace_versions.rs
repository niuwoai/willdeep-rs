//! 工作区内部依赖的版本要求必须跟着工作区版本走。
//!
//! 根 `Cargo.toml` 里 `willdeep-runtime-protocol` / `willdeep-runtime-client` 带着
//! `version = "…"`（发布到 crates.io 要用）。`^0.78.0-rc1` 能匹配 0.78.0 的后续 rc，
//! 匹配不了 0.79.0-rc1：只改 `[workspace.package] version` 的那次提升直接让整个
//! 工作区编译失败（0.79.0-rc1 发生过）。这条测试把两处钉在一起。

#[test]
fn internal_dependency_requirements_match_the_workspace_version() {
    let manifest = include_str!("../../../Cargo.toml");
    let workspace_version = manifest
        .lines()
        .skip_while(|line| line.trim() != "[workspace.package]")
        .find_map(|line| {
            line.trim()
                .strip_prefix("version = \"")
                .and_then(|rest| rest.strip_suffix('"'))
        })
        .expect("[workspace.package] version");
    let mut checked = 0;
    for line in manifest.lines() {
        let line = line.trim();
        if !(line.starts_with("willdeep-") && line.contains("path = \"crates/")) {
            continue;
        }
        let requirement = line
            .split("version = \"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap_or_else(|| panic!("internal dependency without a version: {line}"));
        assert_eq!(
            requirement, workspace_version,
            "bump this requirement together with [workspace.package] version: {line}"
        );
        checked += 1;
    }
    assert!(
        checked >= 2,
        "expected the runtime protocol and client entries"
    );
}
