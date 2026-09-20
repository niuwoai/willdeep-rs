`last_n` 在 n 比切片长的时候会 panic（下标溢出），按文档它应该返回全部元素。修 `src/lib.rs`，`tests/` 不要动，修完跑 `cargo test`。
