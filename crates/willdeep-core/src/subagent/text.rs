//! Byte-bounded truncation shared by the brief builder, the verifier digest
//! and the report the parent finally sees. One rule in one place: cut on a
//! char boundary and say out loud that the tail is gone.

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
/// be read, not streamed wholesale into the parent's transcript.
pub(super) fn bounded_report(value: String) -> String {
    const MAX_REPORT_BYTES: usize = 64 * 1024;
    if value.len() <= MAX_REPORT_BYTES {
        return value;
    }
    let mut boundary = MAX_REPORT_BYTES;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}\n[report truncated]", &value[..boundary])
}
