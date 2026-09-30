//! Byte-bounded truncation shared by the brief builder, the verifier digest
//! and the report the parent finally sees. Cut on a char boundary and say out
//! loud what is gone; reports keep both ends (their conclusion is last).

/// Truncate `value` to `limit` bytes on a char boundary, marking the cut.
pub(super) fn bounded(value: String, limit: usize) -> String {
    if value.len() <= limit {
        return value;
    }
    let mut boundary = limit;
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}\n[truncated]", &value[..boundary])
}

/// Cap the report carried in a lifecycle event. A worker report is meant to
/// be read, not streamed wholesale into the parent's transcript. Keep both
/// ends: the brief-facing opening and the conclusion plus the runtime's
/// `<worker-facts>` trailer, which always come last.
pub(super) fn bounded_report(value: String) -> String {
    const HEAD_BYTES: usize = 48 * 1024;
    const TAIL_BYTES: usize = 16 * 1024;
    crate::background::keep_head_and_tail(&value, HEAD_BYTES, TAIL_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_reports_keep_their_opening_and_their_conclusion() {
        let report = format!(
            "GOAL restated\n{}\nCONCLUSION: fixed it\n<worker-facts verdict=\"passed\">\n</worker-facts>",
            "x".repeat(200 * 1024)
        );
        let bounded = bounded_report(report.clone());
        assert!(bounded.len() < 70 * 1024, "{}", bounded.len());
        assert!(bounded.starts_with("GOAL restated"));
        assert!(bounded.ends_with("</worker-facts>"));
        assert!(bounded.contains("CONCLUSION: fixed it"));
        assert!(bounded.contains("bytes omitted"));
        assert_eq!(bounded_report("short".to_owned()), "short");
        // 多字节字符正好压在切口上也不崩。
        let wide = "字".repeat(40 * 1024);
        assert!(bounded_report(wide).contains("bytes omitted"));
    }
}
