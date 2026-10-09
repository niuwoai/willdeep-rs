# 第一轮候选验证

- 核心测试：671 通过，0 失败，5 忽略。命令：`cargo test --offline -p willdeep-core --lib`。
- Ruby 分析器：5 项测试、12 个断言通过。命令：`ruby scripts/test_swebench_failure_analysis.rb`。
- `cargo fmt --all -- --check` 与 `git diff --check` 通过。
- 历史 DeepSeek 报告 58 个文件 SHA-256 未变；所有公开非空补丁官方判定与索引一致。
- 模型 A/B 尚未运行，未证明解决率提升；候选未发布。
