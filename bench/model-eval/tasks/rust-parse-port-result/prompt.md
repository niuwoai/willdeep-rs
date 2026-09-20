`parse_port` 遇到非法输入直接 panic，按文档它应该返回 `Err`，错误信息里要带上原始输入；另外端口 0 也要算非法。改 `src/lib.rs`，`tests/` 不要动，改完跑 `cargo test`。
