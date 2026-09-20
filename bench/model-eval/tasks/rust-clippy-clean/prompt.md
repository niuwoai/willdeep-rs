`cargo clippy -- -D warnings` 在 `src/lib.rs` 上报了好几条。把警告按 clippy 的建议真正改掉，不许用 `#[allow(...)]` 压掉；函数的行为和签名对调用方要保持兼容，`tests/` 不要动。改完 clippy 和 `cargo test` 都要过。
