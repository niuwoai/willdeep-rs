use std::collections::HashSet;

/// 去重并保留首次出现的顺序；比较大小写敏感。
pub fn dedup_preserving_order(items: &[&str]) -> Vec<String> {
    let mut seen = HashSet::new();
    items
        .iter()
        .filter(|item| seen.insert(**item))
        .map(|item| (*item).to_owned())
        .collect()
}
