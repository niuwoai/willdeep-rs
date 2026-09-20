`src/lib.rs` 里 `Version` 的 `Display` 还是 `todo!()`。把它实现成 `主.次.补丁` 的形式，有预发布标识就接在后面成 `1.2.3-rc4`。只改 `src/lib.rs`，测试在 `tests/display.rs`，不要改，做完跑 `cargo test`。
