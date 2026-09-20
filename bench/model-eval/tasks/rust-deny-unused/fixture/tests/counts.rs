use deny_unused::word_counts;

#[test]
fn counts_in_first_seen_order() {
    let counts = word_counts("b a b c a b");
    assert_eq!(
        counts,
        vec![("b".to_owned(), 3), ("a".to_owned(), 2), ("c".to_owned(), 1)]
    );
}

#[test]
fn empty_text_has_no_words() {
    assert!(word_counts("   ").is_empty());
}
