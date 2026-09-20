use last_n::last_n;

#[test]
fn takes_the_tail() {
    assert_eq!(last_n(&[1, 2, 3], 2), vec![2, 3]);
    assert_eq!(last_n(&[1, 2, 3], 3), vec![1, 2, 3]);
}

#[test]
fn oversized_n_returns_everything() {
    assert_eq!(last_n(&[1, 2], 5), vec![1, 2]);
    assert_eq!(last_n::<i32>(&[], 3), Vec::<i32>::new());
}

#[test]
fn zero_returns_nothing() {
    assert_eq!(last_n(&[1], 0), Vec::<i32>::new());
}
