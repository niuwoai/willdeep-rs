use median_stats::median;

#[test]
fn odd_length_returns_the_middle_value() {
    assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
}

#[test]
fn empty_slice_has_no_median() {
    assert_eq!(median(&[]), None);
}

#[test]
fn even_length_averages_the_middle_pair() {
    assert_eq!(median(&[4.0, 1.0, 3.0, 2.0]), Some(2.5));
}
