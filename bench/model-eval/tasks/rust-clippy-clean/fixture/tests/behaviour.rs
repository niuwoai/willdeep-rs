use clippy_clean::{first_or_zero, is_empty_list, total};

#[test]
fn total_adds_everything() {
    assert_eq!(total(&[1, 2, 3]), 6);
    assert_eq!(total(&[]), 0);
}

#[test]
fn emptiness_is_reported() {
    assert!(is_empty_list(&[]));
    assert!(!is_empty_list(&[7]));
}

#[test]
fn first_falls_back_to_zero() {
    assert_eq!(first_or_zero(&vec![9, 8]), 9);
    assert_eq!(first_or_zero(&Vec::new()), 0);
}
