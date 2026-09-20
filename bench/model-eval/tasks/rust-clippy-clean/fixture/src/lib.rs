/// 求和。
pub fn total(items: &[i32]) -> i32 {
    let mut sum = 0;
    for i in 0..items.len() {
        sum += items[i];
    }
    return sum;
}

/// 是否为空。
pub fn is_empty_list(items: &[i32]) -> bool {
    if items.len() == 0 {
        return true;
    } else {
        return false;
    }
}

/// 第一个元素，没有就 0。
pub fn first_or_zero(items: &Vec<i32>) -> i32 {
    match items.first() {
        Some(value) => *value,
        None => 0,
    }
}
