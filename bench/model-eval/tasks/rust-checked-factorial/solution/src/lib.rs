/// n 的阶乘；结果放不进 u32 时返回 `None`。
pub fn factorial(n: u32) -> Option<u32> {
    (1..=n).try_fold(1u32, |acc, value| acc.checked_mul(value))
}
