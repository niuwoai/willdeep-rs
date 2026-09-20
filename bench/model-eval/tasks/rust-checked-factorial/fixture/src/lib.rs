/// n 的阶乘；结果放不进 u32 时返回 `None`。
pub fn factorial(n: u32) -> Option<u32> {
    Some((1..=n).product())
}
