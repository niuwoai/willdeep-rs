use sum_inclusive::sum_to;

#[test]
fn includes_the_upper_bound() {
    for n in [0, 1, 2, 4, 19, 64, 255] {
        assert_eq!(sum_to(n), n * (n + 1) / 2, "n={n}");
    }
}
