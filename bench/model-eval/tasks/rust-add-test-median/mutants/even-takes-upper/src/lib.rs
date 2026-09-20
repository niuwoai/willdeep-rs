/// 变异：偶数长度不取平均，直接拿上中位。补上偶数长度的测试才能抓住它。
pub fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("no NaN"));
    Some(sorted[sorted.len() / 2])
}
