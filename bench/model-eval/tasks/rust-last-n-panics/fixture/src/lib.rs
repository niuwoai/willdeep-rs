/// 最后 n 个元素；n 超过长度时返回全部。
pub fn last_n<T: Clone>(items: &[T], n: usize) -> Vec<T> {
    items[items.len() - n..].to_vec()
}
