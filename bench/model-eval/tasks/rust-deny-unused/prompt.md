这个 crate 开着 `#![deny(unused)]`，现在编译不过。把没用到的东西清掉让 `cargo test` 变绿；`#![deny(unused)]` 要保留，不许用 `allow` 绕过，`tests/` 不要动。
