/// 求和。
pub fn total(items: &[i32]) -> i32 {
    items.iter().sum()
}

/// 是否为空。
pub fn is_empty_list(items: &[i32]) -> bool {
    items.is_empty()
}

/// 第一个元素，没有就 0。
pub fn first_or_zero(items: &[i32]) -> i32 {
    items.first().copied().unwrap_or(0)
}
