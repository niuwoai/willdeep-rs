use checked_factorial::factorial;

#[test]
fn small_values() {
    assert_eq!(factorial(0), Some(1));
    assert_eq!(factorial(1), Some(1));
    assert_eq!(factorial(5), Some(120));
    assert_eq!(factorial(12), Some(479_001_600));
}

#[test]
fn overflow_is_none_not_garbage() {
    assert_eq!(factorial(13), None);
    assert_eq!(factorial(100), None);
}
