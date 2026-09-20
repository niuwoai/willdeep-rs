#![deny(unused)]

use std::collections::HashMap;

/// 统计每个词出现的次数，按首次出现的顺序返回。
pub fn word_counts(text: &str) -> Vec<(String, usize)> {
    let total = text.split_whitespace().count();
    let mut order: Vec<String> = Vec::new();
    let mut counts: Vec<usize> = Vec::new();
    for word in text.split_whitespace() {
        if let Some(index) = order.iter().position(|seen| seen == word) {
            counts[index] += 1;
        } else {
            order.push(word.to_owned());
            counts.push(1);
        }
    }
    order.into_iter().zip(counts).collect()
}
