use median_stats::median;

#[test]
fn odd_length_returns_the_middle_value() {
    assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
}
