/// 最后 n 个元素；n 超过长度时返回全部。
pub fn last_n<T: Clone>(items: &[T], n: usize) -> Vec<T> {
    let start = items.len().saturating_sub(n);
    items[start..].to_vec()
}
