use version_display::Version;

#[test]
fn renders_three_parts() {
    let version = Version { major: 1, minor: 2, patch: 3, pre: None };
    assert_eq!(version.to_string(), "1.2.3");
    assert_eq!(format!("v{version}"), "v1.2.3");
}

#[test]
fn renders_the_pre_release_suffix() {
    let version = Version { major: 0, minor: 78, patch: 0, pre: Some("rc21".to_owned()) };
    assert_eq!(version.to_string(), "0.78.0-rc21");
}
