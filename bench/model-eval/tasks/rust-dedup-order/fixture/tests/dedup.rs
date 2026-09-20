use dedup_order::dedup_preserving_order;

#[test]
fn keeps_first_occurrences_in_order() {
    assert_eq!(dedup_preserving_order(&["b", "a", "b", "c", "a"]), vec!["b", "a", "c"]);
}

#[test]
fn is_case_sensitive() {
    assert_eq!(dedup_preserving_order(&["A", "a", "A"]), vec!["A", "a"]);
}

#[test]
fn empty_stays_empty() {
    assert!(dedup_preserving_order(&[]).is_empty());
}
