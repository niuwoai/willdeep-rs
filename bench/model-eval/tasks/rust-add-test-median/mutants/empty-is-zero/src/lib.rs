/// 变异：空切片错误地返回 Some(0.0)。补上「空返回 None」的测试才能抓住它。
pub fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return Some(0.0);
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("no NaN"));
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        Some((sorted[mid - 1] + sorted[mid]) / 2.0)
    } else {
        Some(sorted[mid])
    }
}
