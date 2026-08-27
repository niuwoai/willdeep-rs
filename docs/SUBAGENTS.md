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

| 公开 Profile | 用途 | 默认工具 | 窗口 |
|---|---|---|---:|
| `reader` | 阅读和总结长文件或文档 | read / list / search | 48K |
| `implementer` | 有界多文件功能、重构和新文件实现 | search / grep / list / read / create / edit / reviewed shell | 256K |
| `tester` | 测试与行为审核，不改源码 | search / grep / read / git / reviewed shell | 64K |
| `ops_runner` | 有界运维与命令执行 | read / git status / reviewed shell | 48K，最多 32 轮 |
| `judge` | 独立正确性与安全边界审核 | search / grep / read / git diff，无 Shell | 48K |
| `deep` | 无法分片的跨库/超长材料调查 | search / grep / read / list / git status / reviewed shell | 1M（需升级票据） |

旧 ID 不删除，但不再出现在公开工种选择器：`scout` / `reader` / `log_inspector` /
`git_detective` 归并到 Reader 展示组，`editor` / `test_fixer` / `build_fixer` /
`implementer` 归并到 Implementer 展示组。Runtime 仍按原 ID 执行自动路由、托管
工种链、已保存工作流与历史记录，不做会改变旧任务语义的偷偷重映射。

除 `deep` 外，每个工种都跑在 32K / 48K / 64K / 256K 的**可私有部署窗口档位**里，并对工具输出设了独立的字节上限。16K 档已经取消：16GB 显存可部署的 35B-A3B 级模型已经能覆盖至少 32K，继续保留 16K 只会增加路由碎片。详见 [小上下文 Skill Worker](SKILL_WORKERS.md)。

硬性约束：子 Agent **看不到父对话、不能询问用户、不能继续派生**。

外部 Spawn（Runtime API、TUI `/agent spawn`、Web 侧栏）只公开无 Shell、无写入的
`reader` 与 `judge`。命令型、写入型和 `deep` 必须由父 Agent 走完整安全链，外部接口
不能绕过命令审核、写集审批或升级票据。Runtime API 仍接受旧只读 ID，以便已有保存
流程继续运行，但 UI 不再把它们当成公开选项。

### 子 Worker 命令审核与人类兜底

携带 `run_command` 的工种不再被限制为只跑 verifier，但权限也不是“有 Shell 就随便跑”：

1. 确定性静态分类证明为只读或有界的命令直接执行；
2. 非破坏、非凭据敏感且静态无法确认的命令，连同工种、用途和任务摘要交给 AI Safety Judge；
3. Judge 允许才执行；拒绝、不可用或未配置时，子 Worker 返回**原命令**，不会自己弹审批 UI；
4. 父 Agent 可用 `profile = "ops_runner"` 和完全相同的 `target_command` 请求人类一次性授权。任何拼接、加参数、换命令都不继承该授权。

`rm` / force reset 等破坏性形状、内联凭据、SSH/云配置/私钥路径以及环境变量枚举在
进入 AI 前即拒绝。AI 没有为这些命令背书的权力；若业务确实需要，只能走父 Agent
点名完整命令的人类确认。

### Task Packet 与 Verifier

`spawn_agent` 支持可选的结构化参数 `task`（不传时行为与以前完全一致）：主 Agent 把目标、已知事实、约束、只读上下文 `read_files`、写权限 `write_files` 和**验证命令**一次性交给 Worker，Runtime 负责把文件内容内联进 Worker 的第一条消息，并在每次尝试后亲自执行验证命令来判定成败——**Worker 不自证**。

写入型工种（`implementer` / `test_fixer` / `build_fixer`）的可改文件集就是 `task.write_files`，上限 16 个现有或待创建文件，越界写入一律拒绝；`read_files` 只提供上下文。`smart` / `workspace-write` 继承主 Agent 的工作区写权限而免除额外审批；`strict` 仍一次审批整个集合，`read-only` 始终禁止。

每次运行结束都会落盘一条判定（验证结果 / 尝试次数 / 起始 commit），`willdeep daemon agent <id>` 多打一行 `verdict`，`willdeep daemon agent-metrics` 汇总三个派工指标。

完整说明见 [小上下文 Skill Worker](SKILL_WORKERS.md)。

### 模型绑定与预算

各工种可以绑定不同模型，并配置 Token 总预算、执行超时和连续失败熔断：

```toml
[subagents.reader]
# some.im 下省略 provider_profile/model，自动使用 someim-32b-reader。
max_turns = 8
context_window = 49152      # 工种自己的窗口档位，不是会话窗口
tool_output_limit = 5120    # 单次工具输出字节上限
token_budget = 32000
timeout_seconds = 300
max_consecutive_failures = 3

[subagents.deep]
provider_profile = "some-im"
model = "deepseek-v4-flash"
context_window = 1000000
max_turns = 12

[subagents.editor]
provider_profile = "some-im"
model = "glm-5"
max_turns = 6
# 写入型 Profile 默认使用专属可审查 Worktree
worktree = "dedicated"

[subagents.implementer]
# 企业私有部署时绑定本地或内网的 256K Provider/Profile。
provider_profile = "some-im"
model = "glm-5"
max_turns = 18
context_window = 262144
tool_output_limit = 16384
timeout_seconds = 1200
worktree = "dedicated"
```

Provider 为 some.im 时，七个托管窄工种默认使用各自的 `someim-32b-<trade>`，`implementer` 默认使用 GLM-5；`deep` 推荐显式绑定 `deepseek-v4-flash`，且无有效升级票据不会启动。

> **注意**：子 Agent 从 Profile 直接构造 Provider 配置，**不继承** `--api-key` / `WILLDEEP_API_KEY`。给子 Agent 绑定独立 Profile 时必须在该 Profile 里写 `api_key` 或 `api_key_env`，见 [认证与凭据](AUTHENTICATION.md)。

Runtime 和 TUI 会显示轮次、Token 与时限策略。Root / Child Agent 的 input / output / total Token 按身份累计，跨 Session Turn、Child 重试和 Daemon 重启保持。

## `editor` 与 Worktree

`editor` 必须提供 `target_file`。主 Harness 会按当前 Workspace 访问模式解析该文件：`smart/workspace-write` 直接继承工作区写权限，`strict` 单独请求批准；随后创建专属的 `willdeep/agent-<id>` Git Worktree，并把目标文件映射到隔离目录——子 Agent 仍然只能修改这一个文件。

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
willdeep daemon agent-metrics
willdeep daemon stop-agent <agent-id>
willdeep daemon retry-agent <agent-id>
willdeep daemon instruct-agent <agent-id> "补充要求"
```

`agent <id>` 显示单个 Agent 的 Workspace、Profile、状态、轮次、当前工具、Token 和验证判定（是否通过 / 尝试次数 / 起始 commit）。`agent-metrics` 汇总派工指标，口径见 [小上下文 Skill Worker](SKILL_WORKERS.md)。终态 Agent 可以重试，**重试沿用同一 Agent UUID**；`agent.retry` 支持指定新模型，Harness 在重试边界基于原 Provider 配置重建模型实例——运行中的 Agent 不做热切换。

这些命令与其他 Runtime API 一样必须携带私有 `x-willdeep-token`，通过持久队列交给所属的原 Harness 执行并确认。

### TUI

- 右栏 Runtime 区展开后用 `↑` / `↓` 选择 Agent；
- `K` 停止运行中的后台 Child Agent，`R` 重试已结束的；
- `Enter` 打开详情：按 Agent 过滤的最近工具时间线、Workspace Change Artifact、已有结果报告，支持键盘和鼠标滚轮浏览长内容；
- 详情中可补充指令、停止、原模型重试、指定模型重试和查看 Worktree Diff，且不会覆盖 Composer 里已有的草稿；
- `/agent spawn reader|judge <task>` 在活动父会话中创建公开的无 Shell、无写入子 Agent。

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
