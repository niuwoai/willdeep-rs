# 审计导出

> 决策记录：`docs/decisions/2026-09-20-experience-baseline-and-model-eval.md` 第 3 项。
> 企业客户要的不是「有审批」，是一份能拿去看的报告：这一轮 Agent 放行了什么、人拍板了什么、
> hook 拦了什么、验证过了没有、每处改动是谁改的、有没有回滚。`willdeep audit export` 就出这一份。

## 用法

```bash
willdeep audit export                                   # 最近一个会话，Markdown 到 stdout
willdeep audit export --session <uuid>                  # 指定会话
willdeep audit export --workspace . --since 2026-09-14  # 一个工作区最近一周的所有会话
willdeep audit export --json --output audit.json        # JSON 落文件
willdeep --language en audit export                     # 报告语言跟全局 --language / agent.language
```

`--since` / `--until` 接受 `YYYY-MM-DD`、`YYYY-MM-DDTHH:MM:SSZ` 或 Unix 时间戳；不带时区偏移，
不存在的日期直接报错，不猜。不给任何范围时取最近一个会话；给了 `--workspace` 或时间就按条件筛，
筛不到输出一份零会话的报告而不是报错，脚本好判。

它只读 `$WILLDEEP_HOME` 下的状态文件：不需要 Runtime 在跑，不调 Provider，也不走
`AgentStore::open`（那条路会把运行中的 Agent 标成中断——审计不能有副作用）。

## 报告里有什么

| 段 | 来源文件 | 怎么归到会话 |
|---|---|---|
| 汇总 | 下面各段的合计 | |
| 会话 | `sessions/<id>.json` | 标题、工作区、模型、创建 / 更新时间、消息数、检查点状态 / 轮次 / token |
| 审批放行 | `approvals.jsonl` | rc22 起每行带 `session_id`；更早的记录按会话时间窗归入，`matched_by` 标 `time_window` |
| 人工裁决 | `runtime/interactions.json` | 经 `runtime/tasks.json` 的 task → session；审批描述按命令审批同一套规则打码 |
| hook 拦截 | 会话记录里的工具结果 | `<hook-denied hook="…">` 标记，点名 hook 与被拦的工具，理由截到 200 字 |
| 验证证据 | 会话检查点 + `runtime/diff-verifications.json` | 检查点里的要求、基线与证据直接带；快照级记录按本会话改过的快照 id 关联 |
| 子 Agent | `runtime/agents.json` | 只列子级：标签 / 档案、状态、verifier 结论、尝试次数、引用核对、起点 commit、worktree 是否已合并 |
| 改动归属 | `runtime/diff-attributions.json` | 记录自带 `session_id`：时间、根 / 子 Agent、工具、文件、快照 |
| 审阅 / 回滚 | `runtime/diff-reviews.json`、`runtime/recovery/<快照>-*` | 按本会话的快照 id |

JSON 的 `schema_version` 为 1，顶层 `summary` 与每个 `sessions[]` 项的字段名和 Markdown 各段一一对应。

## 口径

- **审批的归入方式要看 `matched_by`。** 有 `session_id` 的记录是确定的；按时间窗归入的旧记录，
  在同一时段并行的两个会话会互相看到对方的——这是旧数据的极限，报告把它们单独计数，不和有键的混在一起。
- **验证计数去重。** 检查点证据和快照级记录常常是同一次验证的两份记录，按（快照，命令）去重后再计数。
- **「没验证」不是「没通过」。** 子 Agent 的 verifier 结论分通过 / 失败 / 未验证三档；验证命令分通过 / 失败 / 其它（超时、没启动起来）。
- **改动归属只认工具调用窗口内真实变化的路径。** 调用前已有的脏文件不算，见 `docs/RUNTIME_DAEMON.md` 的 Diff 一节。
- **报告里没有的东西：** 提示词、模型正文、工具入参、凭据（命令按审批同一套规则打码）、子 Agent 的 verifier 命令行（可能带路径参数，留在 Runtime 私有状态里）。

## 已知缺口

- hook 拦截没有独立日志，只能从会话记录里的工具结果反推；hook 自己收到的载荷在 hook 那边，
  `approval_resolved` 事件也还没接线（见 `docs/HOOKS.md`）。
- 只回滚已跟踪文件的撤销不留痕：`git restore` 之后没有任何文件记得这件事。只有把未跟踪文件
  挪进回收区的撤销能从目录名反推出来。
- `runtime/events.ndjson` 还没并进来：它的会话关联靠消息文本，且 `diff-attributions.json`
  只保留最近 1000 条，久远会话的改动归属可能已被裁掉。
