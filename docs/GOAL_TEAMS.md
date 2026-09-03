# Goal Teams（goal-teams.v1）— CLI 侧引用与落地映射

> Last updated: 2026-08-12 | 状态：引用文档（设计草案，默认假设待确认，确认前不进入编码）
> Canonical 规范：Xedit 仓库 `docs/GOAL_TEAMS_ROLES_DESIGN.md`（`~/Sites/Xedit`）。
> 本文件只做两件事：摘录 willdeep-rs 实现者必须知道的 schema 要点；给出 rs 侧代码锚点与实施步骤。规范冲突时以 canonical 为准。

## 1. 这是什么

对标 Codex 目标模式，为 WillDeep 双端（Xedit macOS App 与本 CLI）引入：

1. **执行期角色注册表**：`developer` / `tester` / `reviewer` 三个跨里程碑存续的成员，
   独立上下文 + 权限收窄 + 证据交付 + 交叉验收（Validator ≠ 本人，禁止自证）；
2. **版本锚定里程碑**：一个 rc 一个里程碑，锁定范围 + 验证方式 + Git 契约（独立提交/tag），
   每 N 版一次周期大门禁（默认 N=40，待确认）；
3. **归档目录规范**：`<workspace>/.willdeep/goal-teams/`，双端读写同一格式（文件层共享，
   与会话文件 / projects.json / mobile-gateway.v1 同一互通模式，不依赖共享运行时协议）；
4. **Doc Capsule 交接契约**：子代理交付物从裸文本升级为结构化 JSON
   （conclusion / evidence / files_touched / constraints_honored / next_suggestion）。

与既有路线图的关系：本设计就是 `docs/CLI_TUI_RUNTIME_ROADMAP.md` §7.7（tester/reviewer
Profile、自定义 Profile）与 §7.8（Agent Team、共享任务看板）的具体化，落地时应更新对应勾选项。

## 2. schema 要点（实现者速查）

### 2.1 新增工种（profile）

| id | 工具白名单 | 说明 |
|---|---|---|
| `tester` | deep 的只读集 + 只读级测试执行 | 跑专项单测/门禁，产出 JSON+Markdown 证据；不改业务实现 |
| `reviewer` | `read_file` `list_directory` `grep_files` `search_files` `git_status` `git_diff` `git_log` `git_blame` | 只读审查改动边界、版本一致性、Git 完整性；禁 shell |

注：`spawn_agent` 外部白名单 `READ_ONLY_TOOLS` 已预留 `git_diff/git_log/git_blame`
（`crates/willdeep-core/src/subagent/catalog.rs:266-281`），`reviewer` 可直接使用。
既有 scout / reader / deep / editor 四工种语义不变。

### 2.2 归档目录

```
<workspace>/.willdeep/goal-teams/
  INDEX.md                     # goal-teams.v1 标识 + 每个 Goal 一行
  versions/<version>/
    plan.md tasklist.md progress.md decisions.md
    spec/inventory.md          # SPEC 与环境盘点表
    capsules/*.json            # Doc Capsule
    qa-notes.md                # tester 跨里程碑记忆（下一版必须注入）
```

### 2.3 Doc Capsule

```json
{
  "schema": "goal-teams.capsule.v1",
  "role": "tester",
  "milestone": "0.22.0-rc3",
  "conclusion": "…",
  "evidence": [{ "kind": "test-report", "path": "…", "summary": "…" }],
  "files_touched": [],
  "constraints_honored": { "read_only": true, "scope": "…", "commands_run": ["…"] },
  "next_suggestion": "…"
}
```

## 3. rs 侧落地步骤与代码锚点（2026-08-12 现状，0.21.0-rc66）

| 步骤 | 内容 | 关键位点 |
|---|---|---|
| R1 | 本引用文档（已落地）；归档目录读写约定 | — |
| R2 | `builtin_profiles` 增加 `tester` / `reviewer`；配置白名单同步放行（**两处必须一起改，否则 TOML 覆盖直接 bail**） | `crates/willdeep-core/src/subagent/profiles.rs:67`（`builtin_profiles`）、`crates/willdeep-cli/src/config.rs:249` |
| R3 | goal / roadmap / teams 对象持久化，替代单字符串 `Session.goal`；顺带修 Web 端 `/goal` 前端内存态刷新即丢的问题 | `crates/willdeep-core/src/session.rs:50`、`crates/willdeep-cli/src/tui.rs:2193-2205`、`crates/willdeep-cli/src/web.rs:351` |
| R4 | capsule outputSchema 机制（当前子代理报告为 final text，64KiB 上限）；`<subagent-report>` 通知升级为 capsule 投递 | `crates/willdeep-core/src/subagent/runner.rs:140`（`run_subagent`）、`crates/willdeep-core/src/subagent/text.rs:19`（`bounded_report`）、`crates/willdeep-core/src/background.rs:391-410` |

步序：R1 → R2 → R3 → R4，每步独立提交、独立验收；默认 Xedit 先行（X1-X4 见 canonical §6.1），
rs 对应跟进。rs 侧已有而 Xedit 缺的 `instruct` 主→子指令通道（`agent.rs:312-325`）保持不动，
将来由 Xedit 反向对齐。

## 4. 待确认假设（与 canonical §7 同步）

周期门禁 N=40；角色命名 developer/tester/reviewer；归档目录 `<workspace>/.willdeep/goal-teams/`；
文档 Markdown、capsule JSON；Xedit 先行。任何一条被推翻，先改 canonical 再同步本文件。

## 5. Non-goals

不合并两端 runtime、不引入共享控制协议（Swift 接入 `/v1/api` 是
`CLI_TUI_RUNTIME_ROADMAP` 的另一条线）；不做子代理独立聊天会话；
不改四个既有工种；Validator 是单一负责人，不是投票器。
