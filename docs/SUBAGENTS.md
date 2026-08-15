# 子 Agent 与后台任务

主 Agent 不必什么都自己干。长文件阅读、跨文件调查、批量定位这类活儿交给隔离上下文的子 Agent，主上下文才不会被撑爆。

## 后台 Shell Job

模型可用 `run_command` 的 `run_in_background = true` 启动后台 Shell，立即获得 `job_xxxxxx`。

任务完成、失败、超时或被取消后，CLI 会把带输出尾部的 `<background-task-notification>` 自动送回主 Harness：主 Agent 空闲时立即续跑，忙碌时在当前回复结束后续跑。

相关工具：

- `get_job_output` — 查看捕获输出；
- `kill_job` — 请求取消。

非 TUI 模式会保持进程存活，直到相关后台任务完成并处理完回流结果。

### 进程与安全边界

后台 Shell 由同版本的内部 Supervisor 托管，通过匿名帧管道接收命令并监视父 Harness 存活：

- 命令**不进入进程参数**，也不进入 Runtime 资源索引；
- 父 Harness 断开或取消时关闭私有存活管道；
- Unix 下在取消、超时或父端断开时终止**独立进程组**，避免留下命令子进程。

后台 Shell 以 `background_shell:<job_id>` 精确绑定 Session、Turn、Task 与 Root Agent。Daemon 重启时收敛为 `Interrupted`，恢复事件只写一次且只含稳定归属 ID。

## 子 Agent Profile

`spawn_agent` 把自包含任务交给隔离上下文的子 Agent，可选择同步等待或 `run_in_background = true`。

| Profile | 用途 | 默认工具 | 窗口 |
|---|---|---|---:|
| `scout` | 快速定位文件、符号和调用点 | search / grep / list / read | 32K |
| `reader` | 阅读和总结长文件或文档 | read / list / search | 32K |
| `log_inspector` | 解释失败日志、归类错误 | read | 16K |
| `git_detective` | 回归定位与 commit 考古 | git log / diff / blame / status / read | 32K |
| `deep` | 跨文件深入调查（父模型） | search / grep / read / list / git status | 继承会话 |
| `editor` | 修改一个明确目标文件 | read / edit | 32K |
| `test_fixer` | 把失败测试修到绿（需 verifier） | read / edit / run_command | 64K |
| `build_fixer` | 修编译 / 类型 / lint 错误（需 verifier） | read / edit / run_command | 32K |

除 `deep` 外，每个工种都跑在**自己的小窗口**里，并对工具输出设了独立的字节上限——给小模型配大窗口不会让它变强，只会让它把窗口烧穿。详见 [小上下文 Skill Worker](SKILL_WORKERS.md)。

硬性约束：子 Agent **看不到父对话、不能询问用户、不能继续派生**。

外部 Spawn（Runtime API、TUI `/agent spawn`、Web 侧栏）只接受 `scout` / `reader` / `deep` / `log_inspector` / `git_detective` 五种**只读** Profile，父级、Task 和 Workspace 全部由 Runtime 推导，调用方不能选择写目标。

### Task Packet 与 Verifier

`spawn_agent` 支持可选的结构化参数 `task`（不传时行为与以前完全一致）：主 Agent 把目标、已知事实、约束、相关文件和**验证命令**一次性交给 Worker，Runtime 负责把文件内容内联进 Worker 的第一条消息，并在每次尝试后亲自执行验证命令来判定成败——**Worker 不自证**。

写入型工种（`test_fixer` / `build_fixer`）的可改文件集就是 `task.relevant_files`，一次审批整个集合，越界写入一律拒绝。

完整说明见 [小上下文 Skill Worker](SKILL_WORKERS.md)。

### 模型绑定与预算

各工种可以绑定不同模型，并配置 Token 总预算、执行超时和连续失败熔断：

```toml
[subagents.scout]
provider_profile = "some-im"
model = "glm-5"
max_turns = 8
context_window = 128000
token_budget = 32000
timeout_seconds = 300
max_consecutive_failures = 3

[subagents.deep]
# 省略 provider_profile/model 时继承当前会话模型
max_turns = 12

[subagents.editor]
provider_profile = "some-im"
model = "glm-5"
max_turns = 6
# 写入型 Profile 默认使用专属可审查 Worktree
worktree = "dedicated"
```

Provider 为 some.im 时，`scout` / `reader` / `editor` 的内置默认模型是 `glm-5`，`deep` 继承父模型。

> **注意**：子 Agent 从 Profile 直接构造 Provider 配置，**不继承** `--api-key` / `WILLDEEP_API_KEY`。给子 Agent 绑定独立 Profile 时必须在该 Profile 里写 `api_key` 或 `api_key_env`，见 [认证与凭据](AUTHENTICATION.md)。

Runtime 和 TUI 会显示轮次、Token 与时限策略。Root / Child Agent 的 input / output / total Token 按身份累计，跨 Session Turn、Child 重试和 Daemon 重启保持。

## `editor` 与 Worktree

`editor` 必须提供 `target_file`。主 Harness 会对 canonicalize 后的**现有文件**单独请求批准；批准后创建专属的 `willdeep/agent-<id>` Git Worktree，并把目标文件映射到隔离目录——子 Agent 仍然只能修改这一个文件。

Worktree 在任务结束后**保留供审查，不会自动删除**。

### 审查与合并

子 Agent 报告会附带有界的 Worktree 路径、分支和变更列表。合并流程是两步确认：

```bash
willdeep daemon agent-worktree-review <AGENT_ID>
willdeep daemon merge-agent-worktree <AGENT_ID> --review <REVIEW_ID> --yes
```

`agent-worktree-review` 获取绑定 Child / Root 精确快照的冲突预检。确认 Review ID 未陈旧后才能执行合并。

TUI 中：在 Agent 详情按 `W` 打开同一审查，再按 `M` 显式批准合并。

以下情况一律阻断，**系统不会自动合并**：

- Root 或 Child 任一侧发生变化；
- 同文件冲突；
- 存在未跟踪文件；
- 存在未解决冲突；
- 补丁过大。

### 审计与隔离

```bash
willdeep daemon worktrees-audit
willdeep daemon quarantine-agent-worktree <AGENT_ID> --snapshot <CHILD_SNAPSHOT_ID> --yes
```

`worktrees-audit` 列出 Active、Reviewable、Merged、Clean、Quarantined、Missing 和 Unknown 状态。

只有**终态且干净**，或已按精确 Child 快照完成合并的 Worktree 才能执行 quarantine。该操作**不会删除目录、文件或分支**，而是通过 `git worktree move` 把完整 Worktree 移入 `~/.willdeep/recovery/worktrees/`。状态持久化失败时会尝试原路回滚。

## 观察与控制

### CLI

```bash
willdeep daemon agents
willdeep daemon agent <agent-id>
willdeep daemon stop-agent <agent-id>
willdeep daemon retry-agent <agent-id>
willdeep daemon instruct-agent <agent-id> "补充要求"
```

`agent <id>` 显示单个 Agent 的 Workspace、Profile、状态、轮次、当前工具和 Token。终态 Agent 可以重试，**重试沿用同一 Agent UUID**；`agent.retry` 支持指定新模型，Harness 在重试边界基于原 Provider 配置重建模型实例——运行中的 Agent 不做热切换。

这些命令与其他 Runtime API 一样必须携带私有 `x-willdeep-token`，通过持久队列交给所属的原 Harness 执行并确认。

### TUI

- 右栏 Runtime 区展开后用 `↑` / `↓` 选择 Agent；
- `K` 停止运行中的后台 Child Agent，`R` 重试已结束的；
- `Enter` 打开详情：按 Agent 过滤的最近工具时间线、Workspace Change Artifact、已有结果报告，支持键盘和鼠标滚轮浏览长内容；
- 详情中可补充指令、停止、原模型重试、指定模型重试和查看 Worktree Diff，且不会覆盖 Composer 里已有的草稿；
- `/agent spawn scout|reader|deep <task>` 在活动父会话中创建只读子 Agent。

Agent 列表保持最小摘要，按 `Enter` 才读取受保护的单项详情。

### Web

Runtime 侧栏可查看 Agent 树、创建只读子 Agent、补充指令、停止和重试，详见 [Web 端指南](WEB_GUIDE.md)。

## 持久化与恢复

Runtime 持久保存子 Agent 的有界最终报告，`willdeep daemon agent` 与 TUI 详情层可审计查看。Prompt 原文不额外持久化或下发。

Root / Child Agent 持久记录实际使用的模型。统一 API、TUI 与 Web 的 Agent 树都展示父子层级、模型、状态、工具、耗时、Token 和 Worktree。

Daemon 重启时的收敛规则：

- 运行中的 Child Agent、Tool 与后台 Shell → `Interrupted`；
- 未应用的 Agent 命令 → `Rejected`；
- 未真正启动的外部 Spawn Child → `Failed`；
- 专属 Worktree 原地保留，供后续 Diff / 合并 / 隔离。

Runtime 持久记录主 Agent 与子 Agent 的 Tool Activity，支持按 Session、Turn、Task、Agent 和状态查询。**公开记录只含工具名与生命周期，不含参数、输出或 Workspace 路径。**

## 相关文档

- [小上下文 Skill Worker](SKILL_WORKERS.md)
- [Runtime Daemon 与工作区](RUNTIME_DAEMON.md)
- [审批与自动化](APPROVALS.md)
- [配置指南](CONFIGURATION.md)
- [Runtime 控制 API](RUNTIME_CONTROL_API.md)
