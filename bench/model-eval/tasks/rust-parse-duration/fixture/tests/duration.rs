use parse_duration::parse_duration;

#[test]
fn parses_single_units() {
    assert_eq!(parse_duration("90s"), Some(90));
    assert_eq!(parse_duration("2m"), Some(120));
    assert_eq!(parse_duration("1h"), Some(3600));
    assert_eq!(parse_duration("0s"), Some(0));
}

#[test]
fn parses_combined_units_in_order() {
    assert_eq!(parse_duration("1h30m"), Some(5400));
    assert_eq!(parse_duration("1h2m3s"), Some(3723));
    assert_eq!(parse_duration("2m30s"), Some(150));
}

#[test]
fn rejects_malformed_input() {
    for text in ["", "abc", "1x", "30", "m5", "1h1h", "30s1m", " 1h"] {
        assert_eq!(parse_duration(text), None, "{text:?}");
    }
}
