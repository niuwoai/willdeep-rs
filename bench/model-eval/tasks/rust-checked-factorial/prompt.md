`factorial(13)` 会因为 u32 溢出 panic，按文档溢出时应该返回 `None`，不能 wrapping 也不能 saturating。修 `src/lib.rs`，`tests/` 不要动，修完跑 `cargo test`。
