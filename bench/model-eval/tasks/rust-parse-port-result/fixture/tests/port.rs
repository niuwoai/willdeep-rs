use parse_port::parse_port;

#[test]
fn accepts_padded_numbers() {
    assert_eq!(parse_port(" 8080 "), Ok(8080));
    assert_eq!(parse_port("65535"), Ok(65535));
}

#[test]
fn rejects_non_numbers_with_the_input_in_the_message() {
    let error = parse_port("http").unwrap_err();
    assert!(error.contains("http"), "{error}");
}

#[test]
fn rejects_out_of_range_and_reserved_ports() {
    assert!(parse_port("70000").unwrap_err().contains("70000"));
    assert!(parse_port("0").is_err());
    assert!(parse_port("").is_err());
}
